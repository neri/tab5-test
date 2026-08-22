//! ESP-Hosted transport over the ESP32-C6's SDIO bus (stage 2 of
//! `docs/WIFI_C6_PLAN.md`).
//!
//! ESP-Hosted multiplexes several logical interfaces over one SDIO link:
//! station and softAP data frames, a serial channel that carries the RPC
//! messages, Bluetooth HCI, and a private channel used only to exchange
//! capabilities right after the slave boots. Every frame is a 12-byte
//! header followed by its payload.
//!
//! The slave side of the link is a set of registers in function 1's address
//! space plus one shared window at `0x1F800`. Reading is
//! "ask how many bytes the slave has produced since boot, then read that
//! many from the window"; writing is "check the slave still has receive
//! buffers, then write into the window". Both counters are free-running and
//! wrap, so this module has to track its own side of them -- losing sync
//! means every later transfer is misaligned.
//!
//! Register layout and the transfer arithmetic are ported from
//! esp-hosted-mcu's `host/drivers/transport/sdio/sdio_drv.c` and
//! `sdio_reg.h`; the handshake mirrors `transport_drv.c`. Unlike that code
//! this module polls instead of taking the SDIO interrupt, and reads
//! registers one byte at a time with CMD52 rather than CMD53 byte mode.

use crate::delay::delay_ms;
use crate::sdio::{self, SdioCard};
use crate::uart;

/// ESP-Hosted's slave registers sit in SDIO function 1.
const FUNCTION: u32 = 1;

// Slave registers. ESP-Hosted spells these as full SLCHOST bus addresses
// (`0x3FF55000 + offset`) and masks them down to 10 bits on the way out;
// these are the masked values.
const REG_INTERRUPT_RAW: u32 = 0x050;
const REG_INTERRUPT_CLEAR: u32 = 0x0D4;
const REG_PACKET_LENGTH: u32 = 0x060;
const REG_TOKEN_READ_DATA: u32 = 0x044;
/// Scratch register 7: writing a bit here raises an interrupt on the slave.
const REG_HOST_TO_SLAVE_INTERRUPT: u32 = 0x08C;

// Bit 23 is "new packet"; this host polls the length register instead of
// gating on it, so only the flow-control bits are acted on here.
const INTERRUPT_START_THROTTLE: u32 = 1 << 7;
const INTERRUPT_STOP_THROTTLE: u32 = 1 << 6;

/// Tells the slave the host is ready to receive (`ESP_OPEN_DATA_PATH`).
const HOST_INTERRUPT_OPEN_DATA_PATH: u8 = 1 << 0;

/// Both directions transfer through a window that *ends* here: a transfer of
/// `n` bytes starts at `0x1F800 - n`.
const TRANSFER_WINDOW_END: u32 = 0x1F800;

/// The slave's byte counter is 20 bits and wraps at this value.
const RX_BYTE_MAX: u32 = 0x0010_0000;
const PACKET_LENGTH_MASK: u32 = 0x000F_FFFF;

/// The slave's receive-buffer counter is 12 bits and wraps at this value.
const TX_BUFFER_MAX: u32 = 0x1000;
const TX_BUFFER_MASK: u32 = 0x0FFF;

/// Size of one slave receive buffer. A frame consumes as many of them as it
/// spans, which is what the token counter is denominated in.
const SLAVE_BUFFER_BYTES: usize = 1536;

/// Largest single frame either direction, header included
/// (`ESP_TRANSPORT_SDIO_MAX_BUF_SIZE`).
pub const MAX_FRAME_BYTES: usize = 1536;
pub const HEADER_BYTES: usize = 12;
pub const MAX_PAYLOAD_BYTES: usize = MAX_FRAME_BYTES - HEADER_BYTES;

/// Room for one read from the slave, which is not the same as one frame:
/// the slave keeps loading frames until the host gets round to reading, and
/// the length register reports the total. A fragmented RPC response arrives
/// as several frames in a single read, so this has to hold the largest
/// message ESP-Hosted will fragment (`MAX_FRAGMENTABLE_PAYLOAD_SIZE`) plus
/// the per-frame headers, rounded to a block.
const STAGING_BYTES: usize = 8704;

// Logical interfaces (`esp_hosted_if_type_t`).
pub const IF_STA: u8 = 1;
/// Carries the RPC messages.
pub const IF_SERIAL: u8 = 3;
/// Carries the capability handshake and nothing else.
pub const IF_PRIV: u8 = 5;

/// Header flag: the payload continues in the next frame.
pub const FLAG_MORE_FRAGMENT: u8 = 1 << 0;

/// Private-channel event types.
const PRIV_EVENT_INIT: u8 = 0x22;

/// TLV tags inside the slave's init event (`ESP_PRIV_TAG_TYPE`).
const TAG_CAPABILITY: u8 = 0x11;
const TAG_FIRMWARE_CHIP_ID: u8 = 0x12;
const TAG_TEST_RAW_THROUGHPUT: u8 = 0x13;
const TAG_RX_QUEUE_SIZE: u8 = 0x14;
const TAG_TX_QUEUE_SIZE: u8 = 0x15;
const TAG_EXTENDED_CAPABILITY: u8 = 0x16;
const TAG_FIRMWARE_VERSION: u8 = 0x17;
const TAG_SDIO_MODE: u8 = 0x18;

/// TLV tags the host answers with (`SLAVE_CONFIG_PRIV_TAG_TYPE`).
const TAG_HOST_CAPABILITY: u8 = 0x44;
const TAG_ECHOED_CHIP_ID: u8 = 0x45;
const TAG_RAW_THROUGHPUT_DIRECTION: u8 = 0x46;
const TAG_THROTTLE_HIGH_THRESHOLD: u8 = 0x47;
const TAG_THROTTLE_LOW_THRESHOLD: u8 = 0x48;

/// Capability bit that says the slave verifies the header's checksum.
const CAPABILITY_CHECKSUM: u8 = 1 << 7;

/// The chip this firmware expects to be talking to
/// (`ESP_PRIV_FIRMWARE_CHIP_ESP32C6`).
pub const CHIP_ID_ESP32C6: u8 = 0x0D;

/// How long to wait for the slave's init event after opening the data path.
const INIT_EVENT_ATTEMPTS: u32 = 200;
const INIT_EVENT_POLL_MS: u32 = 10;

/// How many times a 32-bit register read may disagree with the previous
/// pass before giving up on it.
const REGISTER_READ_ATTEMPTS: u32 = 4;

/// Consecutive failed register reads before the link is declared lost.
const LINK_LOST_AFTER_FAILURES: u32 = 3;

/// How long to wait for the slave to free a receive buffer before dropping a
/// frame.
const TX_BUFFER_ATTEMPTS: u32 = 50;
const TX_BUFFER_POLL_MS: u32 = 2;

/// What the slave reported about itself in its init event.
pub struct SlaveInfo {
    pub chip_id: u8,
    pub capabilities: u8,
    pub extended_capabilities: u32,
    /// `(major << 16) | (minor << 8) | patch`, or 0 if the slave is old
    /// enough not to send the tag.
    pub firmware_version: u32,
    pub rx_queue_size: u8,
    pub tx_queue_size: u8,
    /// The slave streams instead of framing packets. This host only speaks
    /// packet mode, so a streaming slave is a hard mismatch.
    pub streaming_mode: bool,
}

impl SlaveInfo {
    pub fn firmware_major(&self) -> u32 {
        (self.firmware_version >> 16) & 0xFF
    }

    pub fn firmware_minor(&self) -> u32 {
        (self.firmware_version >> 8) & 0xFF
    }

    pub fn firmware_patch(&self) -> u32 {
        self.firmware_version & 0xFF
    }
}

/// One received frame's header fields; the payload is copied out separately.
pub struct Frame {
    pub if_type: u8,
    pub flags: u8,
    pub length: usize,
}

/// A 64-byte-aligned staging buffer. Transfers go through the IDMAC, and the
/// cache maintenance around them works in whole cache lines, so an
/// unaligned buffer would risk dragging neighbouring data along.
#[repr(C, align(64))]
struct DmaBuffer([u8; STAGING_BYTES]);

/// One outgoing frame, aligned for the same reason.
///
/// Separate from the receive staging buffer, and it has to be: a frame can
/// be sent while frames already read from the slave are still waiting to be
/// parsed out of that buffer -- which is exactly what happens when smoltcp
/// answers an ARP request with the transmit token it was handed alongside
/// the received one. Sharing the buffer overwrites the unparsed remainder,
/// and the reader has no way to tell that it happened.
#[repr(C, align(64))]
struct TxBuffer([u8; MAX_FRAME_BYTES]);

/// The host side of the ESP-Hosted link. Owns the counters that have to stay
/// in step with the slave, so there is exactly one of these per activated
/// C6.
pub struct Transport {
    /// The activated card. Held because dropping it would not stop the
    /// transport working, but keeping it makes the ownership obvious and
    /// gives callers the bus details.
    pub card: SdioCard,
    /// Bytes read from the slave so far, modulo [`RX_BYTE_MAX`].
    rx_byte_count: u32,
    /// Slave receive buffers consumed so far, modulo [`TX_BUFFER_MAX`].
    tx_buffer_count: u32,
    sequence: u16,
    checksum_enabled: bool,
    /// The slave asked the host to stop sending station traffic.
    throttled: bool,
    buffer: DmaBuffer,
    tx_buffer: TxBuffer,
    /// How much of `buffer` has already been handed out as frames, and how
    /// much of it is still waiting to be parsed.
    parsed: usize,
    unparsed: usize,
    /// Consecutive failed register reads, and whether that has passed the
    /// point of no return.
    register_failures: u32,
    link_lost: bool,
}

/// Activates the C6 and brings the ESP-Hosted link up to the point where the
/// slave has introduced itself and the host has answered with its own
/// capabilities. From here the serial interface carries RPC.
pub fn bring_up() -> Option<(Transport, SlaveInfo)> {
    let card = sdio::init()?;

    let mut transport = Transport {
        card,
        rx_byte_count: 0,
        tx_buffer_count: 0,
        sequence: 0,
        // Until the slave says otherwise, fill the checksum in: a slave that
        // ignores it does not mind, one that checks it would drop the frame.
        checksum_enabled: true,
        throttled: false,
        buffer: DmaBuffer([0; STAGING_BYTES]),
        tx_buffer: TxBuffer([0; MAX_FRAME_BYTES]),
        parsed: 0,
        unparsed: 0,
        register_failures: 0,
        link_lost: false,
    };

    uart::log(b"HOSTED: opening the data path\r\n");
    if !transport.write_slave_register(REG_HOST_TO_SLAVE_INTERRUPT, HOST_INTERRUPT_OPEN_DATA_PATH) {
        uart::log(b"HOSTED: could not raise the open-data-path interrupt\r\n");
        return None;
    }

    let info = transport.wait_for_init_event()?;
    transport.checksum_enabled = info.capabilities & CAPABILITY_CHECKSUM != 0;

    if info.streaming_mode {
        uart::log(b"HOSTED: slave is in streaming mode, this host only speaks packet mode\r\n");
        return None;
    }
    if info.chip_id != CHIP_ID_ESP32C6 {
        uart::log_hex(b"HOSTED: unexpected slave chip id=", info.chip_id as u32);
    }

    if !transport.send_host_configuration(info.chip_id) {
        uart::log(b"HOSTED: sending the host configuration failed\r\n");
        return None;
    }

    uart::log(b"HOSTED: link is up\r\n");
    Some((transport, info))
}

impl Transport {
    /// Whether the link is still usable. Once the C6 stops answering there
    /// is nothing to be done about it from here -- the card has to be
    /// activated again from scratch -- so callers should drop the transport
    /// rather than keep polling a dead bus.
    pub fn is_alive(&self) -> bool {
        !self.link_lost
    }

    /// Whether the slave has asked the host to hold back station traffic.
    pub fn is_throttled(&self) -> bool {
        self.throttled
    }

    /// Sends one frame. `payload` must fit in [`MAX_PAYLOAD_BYTES`];
    /// splitting anything longer across fragments is the caller's job.
    pub fn send(&mut self, if_type: u8, if_num: u8, flags: u8, payload: &[u8]) -> bool {
        if self.link_lost {
            return false;
        }
        if payload.len() > MAX_PAYLOAD_BYTES {
            uart::log(b"HOSTED: payload too long for one frame\r\n");
            return false;
        }

        let total = HEADER_BYTES + payload.len();
        let buffers_needed = total.div_ceil(SLAVE_BUFFER_BYTES) as u32;
        if !self.wait_for_slave_buffers(buffers_needed) {
            uart::log(b"HOSTED: slave has no receive buffers free\r\n");
            return false;
        }

        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);

        let frame = &mut self.tx_buffer.0;
        frame[..HEADER_BYTES].fill(0);
        frame[0] = (if_type & 0x0F) | (if_num << 4);
        frame[1] = flags;
        frame[2..4].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        frame[4..6].copy_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        frame[8..10].copy_from_slice(&sequence.to_le_bytes());
        frame[HEADER_BYTES..total].copy_from_slice(payload);
        if self.checksum_enabled {
            let checksum = checksum(&frame[..total]);
            frame[6..8].copy_from_slice(&checksum.to_le_bytes());
        }

        // The slave only reads `total` bytes; the rest of the last block is
        // padding it discards, which is what lets this stay a block-mode
        // transfer.
        let padded = total.next_multiple_of(sdio::BLOCK_BYTES);
        frame[total..padded].fill(0);

        if !sdio::transfer_blocks(
            FUNCTION,
            TRANSFER_WINDOW_END - total as u32,
            &mut frame[..padded],
            true,
        ) {
            return false;
        }

        self.tx_buffer_count = (self.tx_buffer_count + buffers_needed) % TX_BUFFER_MAX;
        true
    }

    /// Reads one frame if the slave has produced any, copying its payload
    /// into `payload`. Returns `None` when there is nothing to read, which
    /// is the normal case on an idle link.
    ///
    /// One read from the slave can carry several frames back to back, so
    /// what is already in the staging buffer is drained before the bus is
    /// touched again.
    pub fn receive(&mut self, payload: &mut [u8]) -> Option<Frame> {
        if self.unparsed == 0 && !self.fill_buffer() {
            return None;
        }
        self.take_frame(payload)
    }

    /// Reads whatever the slave has waiting into the staging buffer.
    /// Returns false when there was nothing to read or the read failed.
    ///
    /// The total can span several frames, but each CMD53 asks for at most
    /// one slave buffer: the SDIO slave serves a read out of the buffer the
    /// transfer lands in, and asking for more than that leaves the
    /// controller waiting for data the card never sends -- the FIFO stops
    /// draining and the whole data path wedges. The window ends at a fixed
    /// address, so each chunk is addressed by how much is still outstanding.
    fn fill_buffer(&mut self) -> bool {
        self.service_interrupts();

        let Some(length) = self.pending_length() else {
            return false;
        };
        if length.next_multiple_of(sdio::BLOCK_BYTES) > STAGING_BYTES {
            // Reading less than the slave reported would desync the byte
            // counter for good, so the link cannot recover from here.
            uart::log_hex(
                b"HOSTED: slave has more data than the staging buffer holds, len=",
                length as u32,
            );
            return false;
        }

        let mut remaining = length;
        let mut written = 0usize;
        while remaining > 0 {
            let chunk = remaining.min(SLAVE_BUFFER_BYTES);
            // Only the last chunk can need padding: `SLAVE_BUFFER_BYTES` is
            // itself a whole number of blocks, so every earlier chunk leaves
            // the next one block-aligned.
            let padded = chunk.next_multiple_of(sdio::BLOCK_BYTES);
            if written + padded > STAGING_BYTES {
                uart::log(b"HOSTED: read chunk does not fit the staging buffer\r\n");
                return false;
            }

            if !sdio::transfer_blocks(
                FUNCTION,
                TRANSFER_WINDOW_END - remaining as u32,
                &mut self.buffer.0[written..written + padded],
                false,
            ) {
                return false;
            }
            written += chunk;
            remaining -= chunk;
        }

        // The counter has to advance by exactly what was requested, whether
        // or not the frames themselves turn out to be usable; the slave has
        // already moved on.
        self.rx_byte_count = (self.rx_byte_count + length as u32) % RX_BYTE_MAX;

        self.parsed = 0;
        self.unparsed = length;
        true
    }

    /// Takes the next frame out of the staging buffer. A frame that does not
    /// decode takes the rest of the buffer with it: the frames after it can
    /// only be found by trusting the length of the broken one.
    fn take_frame(&mut self, payload: &mut [u8]) -> Option<Frame> {
        let start = self.parsed;
        let available = self.unparsed;
        let frame = &self.buffer.0[start..start + available];

        if available < HEADER_BYTES {
            uart::log_hex(b"HOSTED: runt frame, len=", available as u32);
            self.unparsed = 0;
            return None;
        }

        let payload_length = u16::from_le_bytes([frame[2], frame[3]]) as usize;
        let offset = u16::from_le_bytes([frame[4], frame[5]]) as usize;
        if offset != HEADER_BYTES || offset + payload_length > available {
            uart::log_hex(b"HOSTED: bad frame offset=", offset as u32);
            uart::log_hex(b"HOSTED: bad frame payload len=", payload_length as u32);
            self.unparsed = 0;
            return None;
        }

        // A slave built without checksums leaves the field at zero, and a
        // real frame never sums to zero (the header alone carries a nonzero
        // interface type), so this adapts to either build instead of relying
        // on the capability bit alone.
        let received = u16::from_le_bytes([frame[6], frame[7]]);
        if self.checksum_enabled && received != 0 {
            let mut header = [0u8; HEADER_BYTES];
            header.copy_from_slice(&frame[..HEADER_BYTES]);
            header[6] = 0;
            header[7] = 0;
            let computed =
                checksum(&header).wrapping_add(checksum(&frame[offset..offset + payload_length]));
            if computed != received {
                uart::log_hex(b"HOSTED: checksum mismatch, got=", received as u32);
                uart::log_hex(b"HOSTED: checksum mismatch, computed=", computed as u32);
                self.unparsed = 0;
                return None;
            }
        }

        if payload_length > payload.len() {
            uart::log_hex(
                b"HOSTED: caller buffer too small for payload=",
                payload_length as u32,
            );
            self.unparsed = 0;
            return None;
        }
        payload[..payload_length].copy_from_slice(&frame[offset..offset + payload_length]);

        let if_type = frame[0] & 0x0F;
        let flags = frame[1];
        let consumed = offset + payload_length;
        self.parsed += consumed;
        self.unparsed -= consumed;
        // Trailing bytes too short to be a header are the padding the slave
        // wrote to fill its last block.
        if self.unparsed < HEADER_BYTES {
            self.unparsed = 0;
        }

        Some(Frame {
            if_type,
            flags,
            length: payload_length,
        })
    }

    /// Reads and clears the slave's interrupt status, keeping the throttle
    /// state up to date. The "new packet" bit is deliberately not used as a
    /// gate: this host polls, and the length register alone already says
    /// whether there is anything to read.
    fn service_interrupts(&mut self) {
        let Some(interrupts) = self.read_slave_register32(REG_INTERRUPT_RAW) else {
            return;
        };
        if interrupts == 0 {
            return;
        }
        self.write_slave_register32(REG_INTERRUPT_CLEAR, interrupts);

        if interrupts & INTERRUPT_START_THROTTLE != 0 {
            self.throttled = true;
        }
        if interrupts & INTERRUPT_STOP_THROTTLE != 0 {
            self.throttled = false;
        }
    }

    /// How many bytes the slave has ready, from the difference between its
    /// free-running byte counter and this host's.
    fn pending_length(&mut self) -> Option<usize> {
        let raw = self.read_slave_register32(REG_PACKET_LENGTH)?;
        if raw == u32::MAX {
            uart::log(b"HOSTED: packet length register reads all ones, SDIO bus fault\r\n");
            return None;
        }

        let produced = raw & PACKET_LENGTH_MASK;
        let length = if produced >= self.rx_byte_count {
            produced - self.rx_byte_count
        } else {
            // The slave's counter wrapped since the last read.
            RX_BYTE_MAX - self.rx_byte_count + produced
        };

        if length == 0 {
            None
        } else {
            Some(length as usize)
        }
    }

    /// Waits until the slave has at least `needed` receive buffers free.
    fn wait_for_slave_buffers(&mut self, needed: u32) -> bool {
        for _ in 0..TX_BUFFER_ATTEMPTS {
            let Some(raw) = self.read_slave_register32(REG_TOKEN_READ_DATA) else {
                return false;
            };
            let produced = (raw >> 16) & TX_BUFFER_MASK;
            let available = (produced + TX_BUFFER_MAX - self.tx_buffer_count) % TX_BUFFER_MAX;
            if available >= needed {
                return true;
            }
            delay_ms(TX_BUFFER_POLL_MS);
        }
        false
    }

    /// Polls the private channel for the slave's init event.
    fn wait_for_init_event(&mut self) -> Option<SlaveInfo> {
        let mut payload = [0u8; MAX_PAYLOAD_BYTES];

        for _ in 0..INIT_EVENT_ATTEMPTS {
            match self.receive(&mut payload) {
                Some(frame) if frame.if_type == IF_PRIV => {
                    if let Some(info) = parse_init_event(&payload[..frame.length]) {
                        return Some(info);
                    }
                }
                Some(frame) => {
                    // Anything else this early is data the slave sent before
                    // the host was ready; there is nowhere to put it yet.
                    uart::log_hex(
                        b"HOSTED: dropping pre-init frame, if_type=",
                        frame.if_type as u32,
                    );
                }
                None => delay_ms(INIT_EVENT_POLL_MS),
            }
        }

        uart::log(b"HOSTED: no init event from the slave\r\n");
        None
    }

    /// Answers the slave's init event with the host's own capabilities, the
    /// chip id it just reported, and the flow-control thresholds. The slave
    /// waits for this before it will serve RPC.
    fn send_host_configuration(&mut self, chip_id: u8) -> bool {
        // Same TLVs, in the same order, as `send_slave_config` in
        // esp-hosted-mcu's `transport_drv.c`. The host capability byte is 0
        // because nothing here needs the slave to change its behaviour.
        let tlvs: [(u8, u8); 5] = [
            (TAG_HOST_CAPABILITY, 0),
            (TAG_ECHOED_CHIP_ID, chip_id),
            (TAG_RAW_THROUGHPUT_DIRECTION, 0),
            (TAG_THROTTLE_HIGH_THRESHOLD, 0),
            (TAG_THROTTLE_LOW_THRESHOLD, 0),
        ];

        let mut event = [0u8; 2 + 3 * 5];
        event[0] = PRIV_EVENT_INIT;
        event[1] = (3 * tlvs.len()) as u8;
        for (index, (tag, value)) in tlvs.into_iter().enumerate() {
            let base = 2 + index * 3;
            event[base] = tag;
            event[base + 1] = 1;
            event[base + 2] = value;
        }

        self.send(IF_PRIV, 0, 0, &event)
    }

    /// Reads a 32-bit slave register as four CMD52 byte reads, repeating
    /// until two passes agree.
    ///
    /// ESP-Hosted's host reads these four bytes in one CMD53 byte-mode
    /// transfer, which cannot tear; on this hardware that transfer never
    /// completed (see `docs/WIFI_C6_PLAN.md`). Byte-at-a-time reads work,
    /// but the packet-length register counts up as the slave produces data,
    /// so a value assembled from four commands can mix an old low half with
    /// a new high half -- and this host's read position is derived from it.
    /// Two matching passes rule that out: the register only changes when the
    /// slave sends something, which is rare next to the time four commands
    /// take.
    fn read_slave_register32(&mut self, address: u32) -> Option<u32> {
        if self.link_lost {
            return None;
        }

        let mut value = None;
        let mut previous = self.read_register32_once(address);
        for _ in 0..REGISTER_READ_ATTEMPTS {
            let current = self.read_register32_once(address);
            if current.is_some() && current == previous {
                value = current;
                break;
            }
            previous = current;
        }

        match value {
            Some(value) => {
                self.register_failures = 0;
                Some(value)
            }
            None => {
                // The C6 answering nothing at all is a different failure
                // from a register that keeps moving: the first means the
                // card is gone, and polling it thousands of times only
                // buries the reason in log noise.
                self.register_failures += 1;
                if self.register_failures >= LINK_LOST_AFTER_FAILURES {
                    self.link_lost = true;
                    uart::log(b"HOSTED: the C6 stopped answering, link lost\r\n");
                    // Whether it still answers CMD5 separates a co-processor
                    // that reset itself from one that is no longer there.
                    sdio::probe_present();
                }
                None
            }
        }
    }

    fn read_register32_once(&self, address: u32) -> Option<u32> {
        let mut value = 0u32;
        for offset in 0..4 {
            let byte = sdio::read_byte(FUNCTION, address + offset)?;
            value |= (byte as u32) << (8 * offset);
        }
        Some(value)
    }

    fn write_slave_register32(&mut self, address: u32, value: u32) -> bool {
        value
            .to_le_bytes()
            .into_iter()
            .enumerate()
            .all(|(offset, byte)| {
                sdio::write_byte(FUNCTION, address + offset as u32, byte).is_some()
            })
    }

    fn write_slave_register(&self, address: u32, value: u8) -> bool {
        sdio::write_byte(FUNCTION, address, value).is_some()
    }
}

/// ESP-Hosted's frame checksum: a plain 16-bit sum of every byte, with the
/// checksum field itself counted as zero.
fn checksum(bytes: &[u8]) -> u16 {
    bytes
        .iter()
        .fold(0u16, |sum, &byte| sum.wrapping_add(byte as u16))
}

/// Parses the slave's init event: an event type and length followed by
/// single-byte-tagged TLVs.
fn parse_init_event(payload: &[u8]) -> Option<SlaveInfo> {
    if payload.len() < 2 || payload[0] != PRIV_EVENT_INIT {
        uart::log(b"HOSTED: private frame that is not an init event\r\n");
        return None;
    }
    let length = payload[1] as usize;
    if 2 + length > payload.len() {
        uart::log(b"HOSTED: init event runs past the frame\r\n");
        return None;
    }

    let mut info = SlaveInfo {
        chip_id: 0,
        capabilities: 0,
        extended_capabilities: 0,
        firmware_version: 0,
        rx_queue_size: 0,
        tx_queue_size: 0,
        streaming_mode: false,
    };

    let tlvs = &payload[2..2 + length];
    let mut position = 0usize;
    while position + 2 <= tlvs.len() {
        let tag = tlvs[position];
        let tag_length = tlvs[position + 1] as usize;
        let value_start = position + 2;
        if value_start + tag_length > tlvs.len() {
            uart::log(b"HOSTED: truncated TLV in the init event\r\n");
            break;
        }
        let value = &tlvs[value_start..value_start + tag_length];

        match tag {
            TAG_CAPABILITY if !value.is_empty() => info.capabilities = value[0],
            TAG_FIRMWARE_CHIP_ID if !value.is_empty() => info.chip_id = value[0],
            TAG_RX_QUEUE_SIZE if !value.is_empty() => info.rx_queue_size = value[0],
            TAG_TX_QUEUE_SIZE if !value.is_empty() => info.tx_queue_size = value[0],
            TAG_SDIO_MODE if !value.is_empty() => info.streaming_mode = value[0] != 0,
            TAG_EXTENDED_CAPABILITY if value.len() >= 4 => {
                info.extended_capabilities =
                    u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
            }
            TAG_FIRMWARE_VERSION if value.len() >= 4 => {
                info.firmware_version =
                    u32::from_le_bytes([value[0], value[1], value[2], value[3]]);
            }
            TAG_TEST_RAW_THROUGHPUT if !value.is_empty() => {
                // Normally zero. A slave built for throughput testing would
                // stream generated data instead of real traffic.
                if value[0] != 0 {
                    uart::log(b"HOSTED: slave has raw throughput testing enabled\r\n");
                }
            }
            _ => uart::log_hex(b"HOSTED: unhandled init TLV tag=", tag as u32),
        }

        position = value_start + tag_length;
    }

    Some(info)
}
