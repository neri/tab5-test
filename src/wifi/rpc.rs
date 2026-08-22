//! ESP-Hosted's RPC layer: remote `esp_wifi_*` calls over the serial
//! interface (stage 3 of `docs/WIFI_C6_PLAN.md`).
//!
//! A call is a protobuf `Rpc` message -- message type, message id, a uid to
//! match the answer with, and one nested payload field whose *field number
//! is the message id* -- wrapped in a small TLV envelope and cut into
//! transport-sized fragments.
//!
//! ```text
//! frame payload: 01 06 00 "RPCRsp" 02 <len16> <protobuf>
//! protobuf:      field 1 = Req|Resp|Event, field 2 = msg id,
//!                field 3 = uid, field <msg id> = the request or response
//! ```
//!
//! Response ids are the request id plus 256 (`Req_Base` 256 against
//! `Resp_Base` 512), and events are their own ids from 768 up. The endpoint
//! name in the envelope is `RPCRsp` in both directions except for events,
//! which use `RPCEvt`; both are six characters, which is what lets the
//! header be a fixed size.
//!
//! Ported from esp-hosted-mcu's `host/drivers/virtual_serial_if/serial_if.c`
//! (the envelope), `host/drivers/serial/serial_ll_if.c` (fragmentation) and
//! `host/drivers/rpc/core/` (the message layout).

use alloc::vec::Vec;

use crate::delay::delay_ms;
use crate::uart;
use crate::wifi::hosted::{
    FLAG_MORE_FRAGMENT, Frame, IF_SERIAL, IF_STA, MAX_PAYLOAD_BYTES, Transport,
};
use crate::wifi::proto::{Reader, Value, Writer};

/// TLV envelope.
const TLV_ENDPOINT: u8 = 0x01;
const TLV_DATA: u8 = 0x02;
const ENDPOINT_RESPONSE: &[u8; 6] = b"RPCRsp";
const ENDPOINT_EVENT: &[u8; 6] = b"RPCEvt";
/// Type byte, 16-bit length, six-character name, type byte, 16-bit length.
const ENVELOPE_BYTES: usize = 1 + 2 + 6 + 1 + 2;

/// `RpcType` values.
const MSG_TYPE_REQUEST: u64 = 1;
const MSG_TYPE_RESPONSE: u64 = 2;
const MSG_TYPE_EVENT: u64 = 3;

/// Field numbers shared by every `Rpc` message.
const FIELD_MSG_TYPE: u32 = 1;
const FIELD_MSG_ID: u32 = 2;
const FIELD_UID: u32 = 3;

/// `Resp_Base - Req_Base`: a response carries its request's id plus this.
const RESPONSE_ID_OFFSET: u32 = 256;

/// Request ids (`RpcId` in `esp_hosted_rpc.proto`).
pub const REQ_GET_MAC_ADDRESS: u32 = 257;

/// A status code the slave returned, i.e. an `esp_err_t` produced by the
/// `esp_wifi_*` call this request stands for. Zero is success.
pub type Status = i32;

/// Largest request this firmware builds, envelope included. Requests are
/// small; the big messages are all responses, which are read into a `Vec`.
const REQUEST_BUFFER_BYTES: usize = 1024;

/// How many frames one [`Rpc::drain`] will read before giving up on
/// catching the slave. Bounded so a busy network cannot pin the shell.
const DRAIN_FRAME_LIMIT: u32 = 64;

/// How long to wait for a response before giving up. Scans block the slave
/// for seconds, so this is generous.
const RESPONSE_TIMEOUT_MS: u32 = 15_000;
const RESPONSE_POLL_MS: u32 = 5;

/// An event the slave pushed while a response was being waited for.
pub struct Event {
    pub msg_id: u32,
    pub payload: Vec<u8>,
}

/// The RPC endpoint on top of an ESP-Hosted transport. Owns the transport
/// because RPC and raw frames cannot be interleaved on the same link
/// without keeping the fragment state consistent.
pub struct Rpc {
    transport: Transport,
    next_uid: u32,
    /// Payload of one received frame, reused across calls.
    frame_payload: Vec<u8>,
    /// Fragments of the message currently being reassembled.
    reassembly: Vec<u8>,
    /// Events that arrived while waiting for a response. Later stages read
    /// these; nothing here interprets them.
    events: Vec<Event>,
    /// Station frames the slave pushed at the host, counted and dropped:
    /// there is no IP stack to hand them to.
    dropped_data_frames: u32,
}

impl Rpc {
    pub fn new(transport: Transport) -> Self {
        Rpc {
            transport,
            // Zero is what an uninitialized field decodes to, so start at
            // one to keep "no uid" distinguishable from the first call.
            next_uid: 1,
            frame_payload: alloc::vec![0; MAX_PAYLOAD_BYTES],
            reassembly: Vec::new(),
            events: Vec::new(),
            dropped_data_frames: 0,
        }
    }

    /// Whether the underlying link is still usable; see
    /// [`Transport::is_alive`].
    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    pub fn dropped_data_frames(&self) -> u32 {
        self.dropped_data_frames
    }

    /// Reads and discards whatever the slave has queued, up to a bounded
    /// number of frames.
    ///
    /// Once a station is associated the co-processor keeps pushing received
    /// frames at the host, and nothing here has an IP stack to give them to.
    /// Left alone they pile up until one read is larger than the staging
    /// buffer, so every command drains the link on its way out.
    pub fn drain(&mut self) {
        for _ in 0..DRAIN_FRAME_LIMIT {
            if self.next_message().is_none() {
                return;
            }
        }
    }

    /// Takes the events collected so far, in arrival order.
    pub fn take_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    /// Sends one request and waits for the matching response, returning the
    /// response's payload message.
    ///
    /// `body` is the already-encoded request message that goes in the field
    /// numbered `request_id`; an empty slice is right for the many requests
    /// that carry no arguments (the field still has to be present, because
    /// that is what selects the union case on the slave).
    pub fn call(&mut self, request_id: u32, body: &[u8]) -> Option<Vec<u8>> {
        let uid = self.next_uid;
        self.next_uid = self.next_uid.wrapping_add(1).max(1);

        let mut buffer = [0u8; REQUEST_BUFFER_BYTES];
        let length = encode_request(&mut buffer, request_id, uid, body)?;
        if !self.send_fragmented(&buffer[..length]) {
            return None;
        }

        self.wait_for_response(request_id + RESPONSE_ID_OFFSET, uid)
    }

    /// Splits an encoded message across as many frames as it needs. Every
    /// fragment but the last carries `MORE_FRAGMENT`.
    fn send_fragmented(&mut self, message: &[u8]) -> bool {
        let mut remaining = message;
        while !remaining.is_empty() {
            let take = remaining.len().min(MAX_PAYLOAD_BYTES);
            let flags = if remaining.len() > take {
                FLAG_MORE_FRAGMENT
            } else {
                0
            };
            if !self.transport.send(IF_SERIAL, 0, flags, &remaining[..take]) {
                return false;
            }
            remaining = &remaining[take..];
        }
        true
    }

    /// Polls the link until the response with this id and uid arrives.
    /// Events and station traffic that turn up meanwhile are set aside
    /// rather than dropped on the floor.
    fn wait_for_response(&mut self, response_id: u32, uid: u32) -> Option<Vec<u8>> {
        for _ in 0..(RESPONSE_TIMEOUT_MS / RESPONSE_POLL_MS) {
            if !self.transport.is_alive() {
                return None;
            }
            while let Some(message) = self.next_message() {
                match message {
                    Decoded {
                        msg_type: MSG_TYPE_RESPONSE,
                        msg_id,
                        uid: response_uid,
                        payload,
                    } => {
                        if msg_id != response_id {
                            uart::log_hex(b"RPC: response for another call, id=", msg_id);
                            continue;
                        }
                        // A slave old enough to predate the uid field echoes
                        // zero. Only one call is ever in flight here, so the
                        // message id alone still identifies the answer.
                        if response_uid == 0 {
                            uart::log(b"RPC: slave did not echo the uid\r\n");
                        } else if response_uid != uid {
                            uart::log_hex(b"RPC: response with a stale uid=", response_uid);
                            continue;
                        }
                        return Some(payload);
                    }
                    Decoded {
                        msg_type: MSG_TYPE_EVENT,
                        msg_id,
                        payload,
                        ..
                    } => self.events.push(Event { msg_id, payload }),
                    other => uart::log_hex(b"RPC: unexpected message type=", other.msg_type as u32),
                }
            }
            delay_ms(RESPONSE_POLL_MS);
        }

        uart::log_hex(b"RPC: timed out waiting for response id=", response_id);
        None
    }

    /// Waits for one of `wanted` to arrive, without sending anything first.
    /// This is how an operation that completes asynchronously on the slave
    /// -- connecting, most of all -- is followed to its end.
    ///
    /// Events that are not being waited for are kept for
    /// [`Rpc::take_events`]; a response arriving here has no caller left to
    /// go to and is dropped with a log line.
    pub fn wait_for_event(&mut self, timeout_ms: u32, wanted: &[u32]) -> Option<Event> {
        for _ in 0..(timeout_ms / RESPONSE_POLL_MS) {
            if !self.transport.is_alive() {
                return None;
            }
            while let Some(message) = self.next_message() {
                if message.msg_type != MSG_TYPE_EVENT {
                    uart::log_hex(b"RPC: dropping a late message, id=", message.msg_id);
                    continue;
                }
                let event = Event {
                    msg_id: message.msg_id,
                    payload: message.payload,
                };
                if wanted.contains(&event.msg_id) {
                    return Some(event);
                }
                self.events.push(event);
            }
            delay_ms(RESPONSE_POLL_MS);
        }
        None
    }

    /// Returns the next complete message the link has ready, or `None` when
    /// nothing more can be read right now.
    fn next_message(&mut self) -> Option<Decoded> {
        while let Some(frame) = self.transport.receive(&mut self.frame_payload) {
            let Some(message) = self.collect(&frame) else {
                continue;
            };
            match decode(&message) {
                Some(decoded) => return Some(decoded),
                None => uart::log(b"RPC: could not decode a message\r\n"),
            }
        }
        None
    }

    /// Feeds one received frame into the reassembly buffer, returning the
    /// whole message once its last fragment has arrived.
    fn collect(&mut self, frame: &Frame) -> Option<Vec<u8>> {
        if frame.if_type != IF_SERIAL {
            if frame.if_type == IF_STA {
                self.dropped_data_frames = self.dropped_data_frames.saturating_add(1);
            } else {
                uart::log_hex(b"RPC: ignoring frame, if_type=", frame.if_type as u32);
            }
            return None;
        }

        self.reassembly
            .extend_from_slice(&self.frame_payload[..frame.length]);
        if frame.flags & FLAG_MORE_FRAGMENT != 0 {
            return None;
        }
        Some(core::mem::take(&mut self.reassembly))
    }
}

/// The parts of an `Rpc` message this layer cares about.
struct Decoded {
    msg_type: u64,
    msg_id: u32,
    uid: u32,
    payload: Vec<u8>,
}

/// Builds envelope and `Rpc` message for one request.
fn encode_request(buffer: &mut [u8], request_id: u32, uid: u32, body: &[u8]) -> Option<usize> {
    let mut message = [0u8; REQUEST_BUFFER_BYTES];
    let mut writer = Writer::new(&mut message);
    writer.uint32_field(FIELD_MSG_TYPE, MSG_TYPE_REQUEST as u32);
    writer.uint32_field(FIELD_MSG_ID, request_id);
    writer.uint32_field(FIELD_UID, uid);
    writer.bytes_field(request_id, body);
    let message_length = writer.finish()?;

    if buffer.len() < ENVELOPE_BYTES + message_length {
        uart::log(b"RPC: request does not fit in the request buffer\r\n");
        return None;
    }

    buffer[0] = TLV_ENDPOINT;
    buffer[1..3].copy_from_slice(&(ENDPOINT_RESPONSE.len() as u16).to_le_bytes());
    buffer[3..9].copy_from_slice(ENDPOINT_RESPONSE);
    buffer[9] = TLV_DATA;
    buffer[10..12].copy_from_slice(&(message_length as u16).to_le_bytes());
    buffer[ENVELOPE_BYTES..ENVELOPE_BYTES + message_length]
        .copy_from_slice(&message[..message_length]);

    Some(ENVELOPE_BYTES + message_length)
}

/// Unwraps the envelope and pulls the header fields and payload out of the
/// `Rpc` message.
fn decode(message: &[u8]) -> Option<Decoded> {
    let body = unwrap_envelope(message)?;

    let mut msg_type = 0u64;
    let mut msg_id = 0u32;
    let mut uid = 0u32;
    // The payload's field number is the message id, which may be read after
    // the payload itself, so remember every candidate until the end.
    let mut fields: Vec<(u32, Vec<u8>)> = Vec::new();

    let mut reader = Reader::new(body);
    while let Some((field, value)) = reader.next_field() {
        match (field, &value) {
            (FIELD_MSG_TYPE, Value::Varint(raw)) => msg_type = *raw,
            (FIELD_MSG_ID, _) => msg_id = value.as_u32(),
            (FIELD_UID, _) => uid = value.as_u32(),
            (_, Value::Bytes(bytes)) => fields.push((field, bytes.to_vec())),
            _ => {}
        }
    }

    if msg_id == 0 {
        uart::log(b"RPC: message without an id\r\n");
        return None;
    }

    let payload = fields
        .into_iter()
        .find(|(field, _)| *field == msg_id)
        .map(|(_, bytes)| bytes)
        .unwrap_or_default();

    Some(Decoded {
        msg_type,
        msg_id,
        uid,
        payload,
    })
}

/// Checks the TLV envelope and returns the protobuf message inside it.
fn unwrap_envelope(message: &[u8]) -> Option<&[u8]> {
    if message.len() < ENVELOPE_BYTES {
        uart::log_hex(
            b"RPC: message shorter than its envelope, len=",
            message.len() as u32,
        );
        return None;
    }
    if message[0] != TLV_ENDPOINT {
        uart::log_hex(
            b"RPC: envelope does not start with an endpoint, got=",
            message[0] as u32,
        );
        return None;
    }

    let name_length = u16::from_le_bytes([message[1], message[2]]) as usize;
    let name = &message[3..3 + ENDPOINT_RESPONSE.len()];
    if name_length != ENDPOINT_RESPONSE.len()
        || (name != ENDPOINT_RESPONSE && name != ENDPOINT_EVENT)
    {
        uart::log(b"RPC: unexpected endpoint name\r\n");
        return None;
    }

    if message[9] != TLV_DATA {
        uart::log_hex(b"RPC: envelope has no data field, got=", message[9] as u32);
        return None;
    }
    let data_length = u16::from_le_bytes([message[10], message[11]]) as usize;
    if ENVELOPE_BYTES + data_length > message.len() {
        uart::log_hex(
            b"RPC: envelope data runs past the message, len=",
            data_length as u32,
        );
        return None;
    }

    Some(&message[ENVELOPE_BYTES..ENVELOPE_BYTES + data_length])
}

/// `Rpc_Req_GetMacAddress { int32 mode = 1 }` / `Rpc_Resp_GetMacAddress
/// { bytes mac = 1, int32 resp = 2 }`.
///
/// `mode` is `WIFI_MODE_STA`, matching what ESP-Hosted's own host passes.
pub const WIFI_MODE_STA: i32 = 1;

/// Asks the slave for one interface's MAC address. Returns the slave's own
/// status code and the address; a nonzero status means the slave refused,
/// most likely because Wi-Fi has not been initialized yet.
pub fn get_mac_address(rpc: &mut Rpc, mode: i32) -> Option<(i32, [u8; 6])> {
    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, mode);
    let length = writer.finish()?;

    let payload = rpc.call(REQ_GET_MAC_ADDRESS, &body[..length])?;

    let mut mac = [0u8; 6];
    let mut status = 0i32;
    let mut reader = Reader::new(&payload);
    while let Some((field, value)) = reader.next_field() {
        match field {
            1 => {
                let bytes = value.as_bytes();
                if let Some(address) = bytes.get(..6) {
                    mac.copy_from_slice(address);
                }
            }
            2 => status = value.as_i32(),
            _ => {}
        }
    }

    Some((status, mac))
}
