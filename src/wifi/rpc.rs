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

use alloc::collections::VecDeque;
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

/// How many frames one [`Rpc::service`] will read before giving up on
/// catching the slave. Bounded so a busy network cannot pin the shell.
const DRAIN_FRAME_LIMIT: u32 = 64;

/// How many received station frames are held for the IP stack.
///
/// Deep enough for a burst of back-to-back frames between two polls of the
/// foreground loop, shallow enough that a stack which stops collecting them
/// cannot eat the heap (32 frames is under 48 KiB of a ~30 MiB heap).
///
/// [`Rpc::service`] stops reading rather than overflowing this, leaving the
/// rest where it is. The paths that cannot stop -- waiting for an RPC
/// response, which must not deadlock behind a queue nobody is draining --
/// drop the *oldest* frame instead: with a retransmitting peer the newest
/// one is the one worth keeping.
const STATION_QUEUE_FRAMES: usize = 32;

/// How many unclaimed events are kept. The link is now serviced from the
/// frame loop rather than only while a command is running, so an AP that
/// keeps associating and dropping would otherwise grow this without end.
const EVENT_QUEUE_LIMIT: usize = 16;

/// How many fill-and-empty rounds [`Rpc::discard_station_frames`] runs
/// before returning, whatever the slave still has waiting.
const DISCARD_ROUNDS: u32 = 4;

/// How long to wait for a response before giving up. Scans block the slave
/// for seconds, so this is generous.
const RESPONSE_TIMEOUT_MS: u32 = 15_000;
const RESPONSE_POLL_MS: u32 = 5;

/// The head of one frame this host handed to the co-processor.
///
/// Kept because the transmit side is the one half of the link that cannot
/// be observed from anywhere else: if the far end sees nothing, only a
/// record made *here* separates "we never built the frame" from "we built
/// it and something downstream swallowed it".
#[derive(Clone, Copy)]
pub struct TransmittedFrame {
    /// The first 14 bytes, which for an 802.3 frame is the whole header.
    pub head: [u8; TRANSMITTED_HEAD_BYTES],
    pub length: usize,
}

pub const TRANSMITTED_HEAD_BYTES: usize = 14;

/// How many transmitted frame heads are remembered.
const TRANSMITTED_HISTORY: usize = 8;

/// How station traffic has fared since the link came up.
///
/// Every field here answers a question that otherwise needs a packet
/// capture on the far side: whether frames are arriving, whether they are
/// reaching the IP stack, and -- the one that is invisible from outside --
/// whether replies are actually leaving.
#[derive(Clone, Copy, Default)]
pub struct DataFrameStats {
    /// Received frames waiting to be collected right now.
    pub queued: usize,
    /// Received frames handed to the IP stack.
    pub delivered: u32,
    /// Received frames that never reached it.
    pub dropped: u32,
    /// Frames handed to the transport for sending.
    pub sent: u32,
    /// Frames not sent because the slave asked for quiet.
    pub throttled: u32,
    /// Frames the transport itself refused.
    pub failed: u32,
    /// Whether the slave is asking for quiet at this moment.
    pub throttling: bool,
}

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
    /// Received station frames waiting for the IP stack, oldest first.
    station_frames: VecDeque<Vec<u8>>,
    /// Station frames that never reached an IP stack: dropped because the
    /// queue was full, or read and thrown away because there was no stack.
    dropped_data_frames: u32,
    delivered_data_frames: u32,
    sent_data_frames: u32,
    /// Heads of the most recently sent station frames, oldest first.
    transmitted: VecDeque<TransmittedFrame>,
    throttled_data_frames: u32,
    failed_data_frames: u32,
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
            station_frames: VecDeque::new(),
            dropped_data_frames: 0,
            delivered_data_frames: 0,
            sent_data_frames: 0,
            transmitted: VecDeque::new(),
            throttled_data_frames: 0,
            failed_data_frames: 0,
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

    pub fn data_frame_stats(&self) -> DataFrameStats {
        DataFrameStats {
            queued: self.station_frames.len(),
            delivered: self.delivered_data_frames,
            dropped: self.dropped_data_frames,
            sent: self.sent_data_frames,
            throttled: self.throttled_data_frames,
            failed: self.failed_data_frames,
            throttling: self.transport.is_throttled(),
        }
    }

    /// Takes the oldest received station frame, if there is one. This is
    /// what `net::device` hands to smoltcp.
    pub fn take_station_frame(&mut self) -> Option<Vec<u8>> {
        let frame = self.station_frames.pop_front();
        if frame.is_some() {
            self.delivered_data_frames = self.delivered_data_frames.saturating_add(1);
        }
        frame
    }

    /// Reads what the slave has waiting and throws the station frames away.
    ///
    /// This is for the state the board spends most of its time in: an
    /// association with no IP stack built yet. The frames have nowhere to
    /// go, and *not* reading them is what eventually kills the link -- the
    /// slave holds them until the pending total no longer fits the
    /// transport's staging buffer, and from there the byte counters cannot
    /// be resynchronized.
    pub fn discard_station_frames(&mut self) {
        for _ in 0..DISCARD_ROUNDS {
            let held_back = self.service();
            let discarded = self.station_frames.len() as u32;
            self.station_frames.clear();
            self.dropped_data_frames = self.dropped_data_frames.saturating_add(discarded);
            if !held_back {
                return;
            }
        }
    }

    /// Sends one 802.3 frame on the station interface.
    ///
    /// Frames are dropped while the slave is throttling: it asked for the
    /// host to hold back, and a dropped frame is a retransmission, whereas
    /// forcing it through is a wedged data path.
    pub fn send_station_frame(&mut self, frame: &[u8]) -> bool {
        if self.transport.is_throttled() {
            self.throttled_data_frames = self.throttled_data_frames.saturating_add(1);
            return false;
        }
        if !self.transport.send(IF_STA, 0, 0, frame) {
            self.failed_data_frames = self.failed_data_frames.saturating_add(1);
            return false;
        }
        self.sent_data_frames = self.sent_data_frames.saturating_add(1);
        self.remember_transmitted(frame);
        true
    }

    /// The most recently sent station frames, oldest first.
    pub fn transmitted_frames(&self) -> impl Iterator<Item = &TransmittedFrame> {
        self.transmitted.iter()
    }

    fn remember_transmitted(&mut self, frame: &[u8]) {
        if self.transmitted.len() >= TRANSMITTED_HISTORY {
            self.transmitted.pop_front();
        }
        let mut head = [0u8; TRANSMITTED_HEAD_BYTES];
        let copied = frame.len().min(TRANSMITTED_HEAD_BYTES);
        head[..copied].copy_from_slice(&frame[..copied]);
        self.transmitted.push_back(TransmittedFrame {
            head,
            length: frame.len(),
        });
    }

    /// Reads whatever the slave has queued, up to a bounded number of
    /// frames, sorting station traffic into the receive queue and events
    /// into [`Rpc::take_events`].
    ///
    /// Once a station is associated the co-processor keeps pushing received
    /// frames at the host. Left alone they pile up until one read is larger
    /// than the staging buffer, so the link has to be serviced regularly
    /// whether or not anything is waiting for an answer.
    ///
    /// Returns true if it stopped with the receive queue full, meaning the
    /// caller should drain the queue and call again rather than assume the
    /// slave has nothing left.
    pub fn service(&mut self) -> bool {
        for _ in 0..DRAIN_FRAME_LIMIT {
            match self.next_message(true) {
                None => break,
                Some(message) if message.msg_type == MSG_TYPE_EVENT => self.push_event(Event {
                    msg_id: message.msg_id,
                    payload: message.payload,
                }),
                Some(message) => {
                    uart::log_hex(b"RPC: dropping an unclaimed message, id=", message.msg_id);
                }
            }
        }
        self.station_frames.len() >= STATION_QUEUE_FRAMES
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
            while let Some(message) = self.next_message(false) {
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
                    } => self.push_event(Event { msg_id, payload }),
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
            while let Some(message) = self.next_message(false) {
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
                self.push_event(event);
            }
            delay_ms(RESPONSE_POLL_MS);
        }
        None
    }

    /// Returns the next complete message the link has ready, or `None` when
    /// nothing more can be read right now.
    ///
    /// With `hold_when_full` the transport is left alone once the station
    /// queue is full: whatever remains stays in the transport's staging
    /// buffer, which is the only way to *not* lose it, since a frame that
    /// has been read cannot be put back. Callers waiting for an RPC answer
    /// pass false -- for them, stopping here would mean waiting forever on
    /// a queue nobody is draining.
    fn next_message(&mut self, hold_when_full: bool) -> Option<Decoded> {
        loop {
            if hold_when_full && self.station_frames.len() >= STATION_QUEUE_FRAMES {
                return None;
            }
            let frame = self.transport.receive(&mut self.frame_payload)?;
            let Some(message) = self.collect(&frame) else {
                continue;
            };
            match decode(&message) {
                Some(decoded) => return Some(decoded),
                None => uart::log(b"RPC: could not decode a message\r\n"),
            }
        }
    }

    /// Feeds one received frame into the reassembly buffer, returning the
    /// whole message once its last fragment has arrived.
    fn collect(&mut self, frame: &Frame) -> Option<Vec<u8>> {
        if frame.if_type != IF_SERIAL {
            if frame.if_type == IF_STA {
                self.queue_station_frame(frame.length);
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

    /// Keeps one event for [`Rpc::take_events`], discarding the oldest
    /// once the queue is full.
    fn push_event(&mut self, event: Event) {
        if self.events.len() >= EVENT_QUEUE_LIMIT {
            self.events.remove(0);
        }
        self.events.push(event);
    }

    /// Copies the station frame just received into the receive queue.
    fn queue_station_frame(&mut self, length: usize) {
        if self.station_frames.len() >= STATION_QUEUE_FRAMES {
            self.station_frames.pop_front();
            self.dropped_data_frames = self.dropped_data_frames.saturating_add(1);
        }
        self.station_frames
            .push_back(self.frame_payload[..length].to_vec());
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

/// `Rpc_Req_GetMacAddress { int32 mode = 0 }` / `Rpc_Resp_GetMacAddress
/// { bytes mac = 1, int32 resp = 2 }`.
///
/// Despite the protobuf field name, the slave passes this value to
/// `esp_wifi_get_mac` as a `wifi_interface_t`. Station is therefore 0;
/// `WIFI_MODE_STA` (1) would select the SoftAP interface instead.
pub const WIFI_IF_STA: i32 = 0;

/// Asks the slave for one interface's MAC address. Returns the slave's own
/// status code and the address; a nonzero status means the slave refused,
/// most likely because Wi-Fi has not been initialized yet.
pub fn get_mac_address(rpc: &mut Rpc, interface: i32) -> Option<(i32, [u8; 6])> {
    let mut body = [0u8; 16];
    let mut writer = Writer::new(&mut body);
    writer.int32_field(1, interface);
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
