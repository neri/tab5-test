//! The slice of protobuf that ESP-Hosted's RPC needs (stage 3 of
//! `docs/WIFI_C6_PLAN.md`).
//!
//! The slave decodes RPC messages with `protobuf-c` generated from
//! `esp_hosted_rpc.proto`, so the wire format is fixed, but only a corner of
//! it is used: varints, length-delimited fields, and nesting. There are no
//! floats, no maps, and no packed repeated fields anywhere in the messages
//! this firmware sends or reads, so this is a few dozen lines rather than a
//! dependency.
//!
//! Both halves are deliberately forgiving in the proto3 way: a writer omits
//! nothing (the slave is happy to receive explicit zeros) and a reader skips
//! fields it does not recognize, which is what lets one decoder cope with
//! whatever extra fields a given slave firmware sends.

/// Wire types. Only these two appear in the messages used here; the reader
/// still has to know the other two to be able to skip them.
const WIRE_VARINT: u32 = 0;
const WIRE_FIXED64: u32 = 1;
const WIRE_LENGTH_DELIMITED: u32 = 2;
const WIRE_FIXED32: u32 = 5;

/// Builds a message into a caller-provided buffer.
///
/// Running out of room is recorded rather than reported at each call:
/// [`Writer::finish`] returns `None` if anything was dropped, so a caller
/// can build a whole message and check once.
pub struct Writer<'a> {
    buffer: &'a mut [u8],
    position: usize,
    overflowed: bool,
}

impl<'a> Writer<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Writer {
            buffer,
            position: 0,
            overflowed: false,
        }
    }

    /// Returns how many bytes were written, or `None` if the buffer was too
    /// small at any point.
    pub fn finish(self) -> Option<usize> {
        if self.overflowed {
            None
        } else {
            Some(self.position)
        }
    }

    /// `int32` fields are sign-extended to 64 bits before encoding, so a
    /// negative value takes the full ten bytes. RSSI thresholds and error
    /// codes go through here.
    pub fn int32_field(&mut self, field: u32, value: i32) {
        self.tag(field, WIRE_VARINT);
        self.varint(value as i64 as u64);
    }

    pub fn uint32_field(&mut self, field: u32, value: u32) {
        self.tag(field, WIRE_VARINT);
        self.varint(value as u64);
    }

    pub fn bool_field(&mut self, field: u32, value: bool) {
        self.tag(field, WIRE_VARINT);
        self.varint(value as u64);
    }

    /// Writes a `bytes`, `string` or nested message field: all three are
    /// length-delimited and differ only in how the payload is produced.
    pub fn bytes_field(&mut self, field: u32, value: &[u8]) {
        self.tag(field, WIRE_LENGTH_DELIMITED);
        self.varint(value.len() as u64);
        for &byte in value {
            self.push(byte);
        }
    }

    fn tag(&mut self, field: u32, wire_type: u32) {
        self.varint(((field as u64) << 3) | wire_type as u64);
    }

    fn varint(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 0x7F) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.push(byte);
            if value == 0 {
                return;
            }
        }
    }

    fn push(&mut self, byte: u8) {
        match self.buffer.get_mut(self.position) {
            Some(slot) => {
                *slot = byte;
                self.position += 1;
            }
            None => self.overflowed = true,
        }
    }
}

/// One field's value, as far as the wire format describes it. The two
/// fixed-width forms carry nothing: no message used here has such a field,
/// and the reader only needs to recognize them well enough to step over
/// one.
pub enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

impl<'a> Value<'a> {
    /// Reads the value as a protobuf `int32`. Negative values arrive
    /// sign-extended to 64 bits, so the low half is the answer.
    pub fn as_i32(&self) -> i32 {
        match self {
            Value::Varint(value) => *value as u32 as i32,
            _ => 0,
        }
    }

    pub fn as_u32(&self) -> u32 {
        match self {
            Value::Varint(value) => *value as u32,
            _ => 0,
        }
    }

    /// The borrow is of the message being read, not of this value, so a
    /// caller can keep a nested message around while it walks on.
    pub fn as_bytes(&self) -> &'a [u8] {
        match self {
            Value::Bytes(bytes) => bytes,
            _ => &[],
        }
    }
}

/// Walks the fields of one message.
pub struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, position: 0 }
    }

    /// Returns the next `(field number, value)` pair, or `None` at the end
    /// of the message or on anything malformed. A truncated message and a
    /// complete one are not distinguished: the caller checks that it found
    /// the fields it needs either way.
    pub fn next_field(&mut self) -> Option<(u32, Value<'a>)> {
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let wire_type = (key & 0x7) as u32;

        let value = match wire_type {
            WIRE_VARINT => Value::Varint(self.varint()?),
            WIRE_LENGTH_DELIMITED => {
                let length = self.varint()? as usize;
                let end = self.position.checked_add(length)?;
                let bytes = self.data.get(self.position..end)?;
                self.position = end;
                Value::Bytes(bytes)
            }
            WIRE_FIXED32 => {
                self.skip(4)?;
                Value::Fixed
            }
            WIRE_FIXED64 => {
                self.skip(8)?;
                Value::Fixed
            }
            _ => return None,
        };

        Some((field, value))
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.data.get(self.position)?;
            self.position += 1;
            value |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        let end = self.position.checked_add(count)?;
        if end > self.data.len() {
            return None;
        }
        self.position = end;
        Some(())
    }
}
