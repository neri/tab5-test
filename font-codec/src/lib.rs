//! Minimal LZ4 raw-block container used only for boot-time font expansion.
//!
//! The firmware decoder has no allocator, frame state or external dictionary.
//! The optional encoder is build-script-only and intentionally favours a
//! straightforward reproducible implementation over maximum compression.

#![no_std]

#[cfg(feature = "encoder")]
extern crate alloc;

pub const HEADER_BYTES: usize = 28;
const MAGIC: &[u8; 4] = b"T5L4";
const VERSION: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    HeaderTruncated,
    BadMagic,
    BadVersion,
    BadHeaderLength,
    LengthOverflow,
    InputLengthMismatch,
    OutputLengthMismatch,
    CompressedCrc,
    RawCrc,
    TokenTruncated,
    LiteralLengthOverflow,
    LiteralInputOverrun,
    LiteralOutputOverrun,
    OffsetTruncated,
    ZeroOffset,
    OffsetBeforeOutput,
    MatchLengthOverflow,
    MatchOutputOverrun,
    MissingFinalLiteralSequence,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

pub fn decoded_len(container: &[u8]) -> Result<usize, DecodeError> {
    validate_header(container).map(|header| header.raw_len)
}

struct Header {
    raw_len: usize,
    block_len: usize,
    raw_crc: u32,
    block_crc: u32,
}

fn validate_header(container: &[u8]) -> Result<Header, DecodeError> {
    if container.len() < HEADER_BYTES {
        return Err(DecodeError::HeaderTruncated);
    }
    if &container[..4] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    if u16_at(container, 4) != VERSION {
        return Err(DecodeError::BadVersion);
    }
    if u16_at(container, 6) as usize != HEADER_BYTES {
        return Err(DecodeError::BadHeaderLength);
    }
    let raw_len = u32_at(container, 8) as usize;
    let block_len = u32_at(container, 12) as usize;
    let total = HEADER_BYTES
        .checked_add(block_len)
        .ok_or(DecodeError::LengthOverflow)?;
    if total != container.len() {
        return Err(DecodeError::InputLengthMismatch);
    }
    if u32_at(container, 24) != 0 {
        return Err(DecodeError::BadVersion);
    }
    Ok(Header {
        raw_len,
        block_len,
        raw_crc: u32_at(container, 16),
        block_crc: u32_at(container, 20),
    })
}

fn extended_length(
    input: &[u8],
    cursor: &mut usize,
    base: usize,
    overflow: DecodeError,
) -> Result<usize, DecodeError> {
    let mut length = base;
    if base != 15 {
        return Ok(length);
    }
    loop {
        let byte = *input.get(*cursor).ok_or(DecodeError::TokenTruncated)?;
        *cursor += 1;
        length = length.checked_add(byte as usize).ok_or(overflow)?;
        if byte != 255 {
            return Ok(length);
        }
    }
}

/// Expands one complete T5L4 container into its exact caller-owned buffer.
pub fn decode(container: &[u8], output: &mut [u8]) -> Result<(), DecodeError> {
    let header = validate_header(container)?;
    if output.len() != header.raw_len {
        return Err(DecodeError::OutputLengthMismatch);
    }
    let block = &container[HEADER_BYTES..HEADER_BYTES + header.block_len];
    if crc32(block) != header.block_crc {
        return Err(DecodeError::CompressedCrc);
    }

    let (mut input_cursor, mut output_cursor) = (0usize, 0usize);
    loop {
        let token = *block.get(input_cursor).ok_or(DecodeError::TokenTruncated)?;
        input_cursor += 1;
        let literal_length = extended_length(
            block,
            &mut input_cursor,
            (token >> 4) as usize,
            DecodeError::LiteralLengthOverflow,
        )?;
        let literal_end = input_cursor
            .checked_add(literal_length)
            .ok_or(DecodeError::LiteralLengthOverflow)?;
        if literal_end > block.len() {
            return Err(DecodeError::LiteralInputOverrun);
        }
        let output_end = output_cursor
            .checked_add(literal_length)
            .ok_or(DecodeError::LiteralLengthOverflow)?;
        if output_end > output.len() {
            return Err(DecodeError::LiteralOutputOverrun);
        }
        output[output_cursor..output_end].copy_from_slice(&block[input_cursor..literal_end]);
        input_cursor = literal_end;
        output_cursor = output_end;

        if input_cursor == block.len() {
            if output_cursor != output.len() {
                return Err(DecodeError::OutputLengthMismatch);
            }
            break;
        }
        if input_cursor + 2 > block.len() {
            return Err(DecodeError::OffsetTruncated);
        }
        let offset = u16_at(block, input_cursor) as usize;
        input_cursor += 2;
        if offset == 0 {
            return Err(DecodeError::ZeroOffset);
        }
        if offset > output_cursor {
            return Err(DecodeError::OffsetBeforeOutput);
        }
        let match_base = (token & 0x0f) as usize;
        let match_length = extended_length(
            block,
            &mut input_cursor,
            match_base,
            DecodeError::MatchLengthOverflow,
        )?
        .checked_add(4)
        .ok_or(DecodeError::MatchLengthOverflow)?;
        let match_end = output_cursor
            .checked_add(match_length)
            .ok_or(DecodeError::MatchLengthOverflow)?;
        if match_end > output.len() {
            return Err(DecodeError::MatchOutputOverrun);
        }
        for _ in 0..match_length {
            output[output_cursor] = output[output_cursor - offset];
            output_cursor += 1;
        }
        if output_cursor == output.len() {
            return Err(DecodeError::MissingFinalLiteralSequence);
        }
    }
    if crc32(output) != header.raw_crc {
        return Err(DecodeError::RawCrc);
    }
    Ok(())
}

#[cfg(feature = "encoder")]
mod encoder {
    use super::{HEADER_BYTES, MAGIC, VERSION, crc32};
    use alloc::vec;
    use alloc::vec::Vec;

    const HASH_BITS: usize = 16;
    const HASH_SIZE: usize = 1 << HASH_BITS;
    const INVALID: usize = usize::MAX;

    fn hash(bytes: &[u8], at: usize) -> usize {
        let value = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
        (value.wrapping_mul(0x9e37_79b1) >> (32 - HASH_BITS)) as usize
    }

    fn emit_length(output: &mut Vec<u8>, mut extra: usize) {
        while extra >= 255 {
            output.push(255);
            extra -= 255;
        }
        output.push(extra as u8);
    }

    fn emit_sequence(
        output: &mut Vec<u8>,
        literals: &[u8],
        offset: Option<usize>,
        match_length: usize,
    ) {
        let literal_nibble = literals.len().min(15);
        let match_code = offset.map_or(0, |_| match_length - 4);
        let match_nibble = match_code.min(15);
        output.push(((literal_nibble as u8) << 4) | match_nibble as u8);
        if literals.len() >= 15 {
            emit_length(output, literals.len() - 15);
        }
        output.extend_from_slice(literals);
        if let Some(offset) = offset {
            output.extend_from_slice(&(offset as u16).to_le_bytes());
            if match_code >= 15 {
                emit_length(output, match_code - 15);
            }
        }
    }

    pub fn encode_block(input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut table = vec![INVALID; HASH_SIZE];
        let (mut anchor, mut cursor) = (0usize, 0usize);
        let match_search_end = input.len().saturating_sub(12);
        while cursor <= match_search_end && cursor + 4 <= input.len() {
            let slot = hash(input, cursor);
            let candidate = table[slot];
            table[slot] = cursor;
            let usable = candidate != INVALID
                && cursor - candidate <= u16::MAX as usize
                && input[candidate..candidate + 4] == input[cursor..cursor + 4];
            if !usable {
                cursor += 1;
                continue;
            }
            let max_end = input.len() - 5;
            let mut end = cursor + 4;
            while end < max_end && input[candidate + (end - cursor)] == input[end] {
                end += 1;
            }
            emit_sequence(
                &mut output,
                &input[anchor..cursor],
                Some(cursor - candidate),
                end - cursor,
            );
            let old_cursor = cursor;
            cursor = end;
            anchor = end;
            let mut update = old_cursor + 1;
            while update + 4 <= cursor && update + 4 <= input.len() {
                table[hash(input, update)] = update;
                update += 1;
            }
        }
        emit_sequence(&mut output, &input[anchor..], None, 0);
        output
    }

    pub fn encode_container(input: &[u8]) -> Vec<u8> {
        assert!(u32::try_from(input.len()).is_ok());
        let block = encode_block(input);
        assert!(u32::try_from(block.len()).is_ok());
        let mut output = Vec::with_capacity(HEADER_BYTES + block.len());
        output.extend_from_slice(MAGIC);
        output.extend_from_slice(&VERSION.to_le_bytes());
        output.extend_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        output.extend_from_slice(&(input.len() as u32).to_le_bytes());
        output.extend_from_slice(&(block.len() as u32).to_le_bytes());
        output.extend_from_slice(&crc32(input).to_le_bytes());
        output.extend_from_slice(&crc32(&block).to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());
        output.extend_from_slice(&block);
        output
    }
}

#[cfg(feature = "encoder")]
pub use encoder::{encode_block, encode_container};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    extern crate alloc;

    fn container(block: &[u8], raw_len: usize, raw_crc: u32) -> alloc::vec::Vec<u8> {
        let mut bytes = alloc::vec::Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(&(HEADER_BYTES as u16).to_le_bytes());
        bytes.extend_from_slice(&(raw_len as u32).to_le_bytes());
        bytes.extend_from_slice(&(block.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&raw_crc.to_le_bytes());
        bytes.extend_from_slice(&crc32(block).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(block);
        bytes
    }

    #[test]
    fn literals_and_overlapping_matches_decode() {
        // "abc" literals followed by offset=3, match length=9, then five
        // final literals. The match overlaps its own newly written output.
        let block = [
            0x35, b'a', b'b', b'c', 3, 0, 0x50, b'1', b'2', b'3', b'4', b'5',
        ];
        let expected = b"abcabcabcabc12345";
        let encoded = container(&block, expected.len(), crc32(expected));
        let mut output = vec![0; expected.len()];
        decode(&encoded, &mut output).unwrap();
        assert_eq!(&output, expected);
    }

    #[test]
    fn every_truncation_is_rejected() {
        let raw = b"abcdefghijklmnopqrstuvwxyz";
        let block = [
            0xf0, 11, b'a', b'b', b'c', b'd', b'e', b'f', b'g', b'h', b'i', b'j', b'k', b'l', b'm',
            b'n', b'o', b'p', b'q', b'r', b's', b't', b'u', b'v', b'w', b'x', b'y', b'z',
        ];
        let encoded = container(&block, raw.len(), crc32(raw));
        for length in 0..encoded.len() {
            let mut output = vec![0; raw.len()];
            assert!(decode(&encoded[..length], &mut output).is_err(), "{length}");
        }
    }

    #[test]
    fn bad_offsets_and_output_overruns_are_rejected() {
        for (block, error) in [
            (
                &[0x10, b'a', 0, 0, 0x50, b'1', b'2', b'3', b'4', b'5'][..],
                DecodeError::ZeroOffset,
            ),
            (
                &[0x10, b'a', 2, 0, 0x50, b'1', b'2', b'3', b'4', b'5'][..],
                DecodeError::OffsetBeforeOutput,
            ),
            (
                &[0x1f, b'a', 1, 0, 255, 0, 0x50, b'1', b'2', b'3', b'4', b'5'][..],
                DecodeError::MatchOutputOverrun,
            ),
        ] {
            let encoded = container(block, 16, 0);
            let mut output = [0; 16];
            assert_eq!(decode(&encoded, &mut output), Err(error));
        }
    }

    #[test]
    fn wrapper_lengths_and_both_checksums_are_enforced() {
        let raw = b"font data";
        let block = [0x90, b'f', b'o', b'n', b't', b' ', b'd', b'a', b't', b'a'];
        let encoded = container(&block, raw.len(), crc32(raw));

        let mut wrong_output = [0; 8];
        assert_eq!(
            decode(&encoded, &mut wrong_output),
            Err(DecodeError::OutputLengthMismatch)
        );

        let mut corrupt_block = encoded.clone();
        corrupt_block[HEADER_BYTES + 1] ^= 1;
        let mut output = [0; 9];
        assert_eq!(
            decode(&corrupt_block, &mut output),
            Err(DecodeError::CompressedCrc)
        );

        let mut wrong_raw_crc = encoded;
        wrong_raw_crc[16] ^= 1;
        assert_eq!(
            decode(&wrong_raw_crc, &mut output),
            Err(DecodeError::RawCrc)
        );
    }

    #[cfg(feature = "encoder")]
    #[test]
    fn encoder_round_trips_boundaries_repetition_and_noise() {
        let mut noise = [0u8; 1024];
        let mut state = 0x1234_5678u32;
        for byte in &mut noise {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        for raw in [
            &b""[..],
            &b"a"[..],
            &b"abcd"[..],
            &b"abcabcabcabcabcabc"[..],
            &b"00000000000000000000000000000000"[..],
            &noise[..],
        ] {
            let encoded = encode_container(raw);
            let mut output = vec![0; raw.len()];
            decode(&encoded, &mut output).unwrap();
            assert_eq!(output, raw);
        }
    }
}
