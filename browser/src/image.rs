//! Incremental image header inspection. It never allocates and never assumes
//! dimensions occur in a fixed prefix; callers feed the bytes accumulated so
//! far and keep reading only while `NeedMore` is returned.

use alloc::vec::Vec;

use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::bytestream::ZCursor;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::limits::{
    MAX_IMAGE_COMPRESSED_BYTES, MAX_IMAGE_DECODE_WORK_BYTES, MAX_IMAGE_HEIGHT, MAX_IMAGE_PIXELS,
    MAX_IMAGE_WIDTH,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    WebP,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dimensions {
    pub format: Format,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inspection {
    NeedMore,
    Dimensions(Dimensions),
    Unsupported,
    Malformed,
    TooLarge,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DecodeError {
    Unsupported,
    Malformed,
    TooLarge,
    OutOfMemory,
}

#[derive(Debug)]
pub struct DecodedImage {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u16>,
}

/// PNG CRC-32 over the four-byte chunk type followed by its data.
pub fn png_chunk_crc(kind: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

/// Decodes the supported image format after checking its signature.
pub fn decode(bytes: &[u8]) -> Result<DecodedImage, DecodeError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(bytes)
    } else if bytes.starts_with(b"\xff\xd8") {
        decode_jpeg(bytes)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        decode_webp(bytes)
    } else {
        Err(DecodeError::Unsupported)
    }
}

/// Decodes a still WebP image. Animated WebP is deliberately rejected before
/// entering the one-shot decoder (whose normal API returns its first frame).
/// The existing decode-work budget limits the temporary RGBA8 buffer.
pub fn decode_webp(bytes: &[u8]) -> Result<DecodedImage, DecodeError> {
    if bytes.len() > MAX_IMAGE_COMPRESSED_BYTES {
        return Err(DecodeError::TooLarge);
    }
    if !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return Err(DecodeError::Unsupported);
    }
    match webpkit::is_animated(bytes) {
        Ok(true) => return Err(DecodeError::Unsupported),
        Ok(false) => {}
        Err(error) => return Err(map_webp_error(error)),
    }
    let max_pixels = (MAX_IMAGE_PIXELS.min(MAX_IMAGE_DECODE_WORK_BYTES / 4)) as u64;
    let options = webpkit::DecodeOptions::new()
        .max_pixels(max_pixels)
        .read_metadata(false);
    let rgba = webpkit::decode_with(bytes, &options).map_err(map_webp_error)?;
    let width = rgba.width();
    let height = rgba.height();
    match dimensions(Format::WebP, width, height) {
        Inspection::Dimensions(_) => {}
        Inspection::TooLarge => return Err(DecodeError::TooLarge),
        _ => return Err(DecodeError::Malformed),
    }
    let pixel_count = usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(DecodeError::TooLarge)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(pixel_count)
        .map_err(|_| DecodeError::OutOfMemory)?;
    for pixel in rgba.as_bytes().chunks_exact(4) {
        pixels.push(rgb565_over_white(pixel[0], pixel[1], pixel[2], pixel[3]));
    }
    if pixels.len() != pixel_count {
        return Err(DecodeError::Malformed);
    }
    Ok(DecodedImage {
        width: width as u16,
        height: height as u16,
        pixels,
    })
}

fn map_webp_error(error: webpkit::Error) -> DecodeError {
    match error {
        webpkit::Error::LimitExceeded { .. } => DecodeError::TooLarge,
        webpkit::Error::UnsupportedFeature => DecodeError::Unsupported,
        _ => DecodeError::Malformed,
    }
}

/// Decodes baseline 8-bit JPEG with either one grayscale component or three
/// color components. Progressive and extended JPEG variants are rejected
/// before entering the decoder so their larger work profiles cannot surprise
/// the frame loop.
pub fn decode_jpeg(bytes: &[u8]) -> Result<DecodedImage, DecodeError> {
    if bytes.len() > MAX_IMAGE_COMPRESSED_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let (width, height) = baseline_jpeg_dimensions(bytes)?;
    let rgb_size = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or(DecodeError::TooLarge)?;
    if rgb_size > MAX_IMAGE_DECODE_WORK_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let options = DecoderOptions::new_safe()
        .set_max_width(MAX_IMAGE_WIDTH as usize)
        .set_max_height(MAX_IMAGE_HEIGHT as usize)
        .set_strict_mode(true)
        .jpeg_set_out_colorspace(ColorSpace::RGB);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    let rgb = decoder.decode().map_err(|_| DecodeError::Malformed)?;
    if rgb.len() != rgb_size {
        return Err(DecodeError::Malformed);
    }
    let pixel_count = width.checked_mul(height).ok_or(DecodeError::TooLarge)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(pixel_count)
        .map_err(|_| DecodeError::OutOfMemory)?;
    for pixel in rgb.chunks_exact(3) {
        pixels.push(
            ((pixel[0] as u16 & 0xf8) << 8)
                | ((pixel[1] as u16 & 0xfc) << 3)
                | (pixel[2] as u16 >> 3),
        );
    }
    Ok(DecodedImage {
        width: width as u16,
        height: height as u16,
        pixels,
    })
}

fn baseline_jpeg_dimensions(bytes: &[u8]) -> Result<(usize, usize), DecodeError> {
    if !bytes.starts_with(b"\xff\xd8") {
        return Err(DecodeError::Unsupported);
    }
    let mut offset = 2usize;
    loop {
        if offset >= bytes.len() || bytes[offset] != 0xff {
            return Err(DecodeError::Malformed);
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        let marker = *bytes.get(offset).ok_or(DecodeError::Malformed)?;
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            return Err(DecodeError::Malformed);
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length_bytes = bytes
            .get(offset..offset + 2)
            .ok_or(DecodeError::Malformed)?;
        let length = u16::from_be_bytes([length_bytes[0], length_bytes[1]]) as usize;
        if length < 2 {
            return Err(DecodeError::Malformed);
        }
        let end = offset.checked_add(length).ok_or(DecodeError::Malformed)?;
        if end > bytes.len() {
            return Err(DecodeError::Malformed);
        }
        if is_sof(marker) {
            if marker != 0xc0 {
                return Err(DecodeError::Unsupported);
            }
            if length < 8 || bytes[offset + 2] != 8 {
                return Err(if length < 8 {
                    DecodeError::Malformed
                } else {
                    DecodeError::Unsupported
                });
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as usize;
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as usize;
            let components = bytes[offset + 7];
            if !matches!(components, 1 | 3) {
                return Err(DecodeError::Unsupported);
            }
            return match dimensions(Format::Jpeg, width as u32, height as u32) {
                Inspection::Dimensions(_) => Ok((width, height)),
                Inspection::TooLarge => Err(DecodeError::TooLarge),
                _ => Err(DecodeError::Malformed),
            };
        }
        offset = end;
    }
}

const PNG_GREYSCALE: u8 = 0;
const PNG_RGB: u8 = 2;
const PNG_PALETTE: u8 = 3;
const PNG_GREYSCALE_ALPHA: u8 = 4;
const PNG_RGBA: u8 = 6;

#[derive(Clone, Copy)]
struct PngHeader {
    width: usize,
    height: usize,
    bit_depth: u8,
    color_type: u8,
}

impl PngHeader {
    /// Checks the IHDR fields that follow width and height.
    ///
    /// A colour type and bit depth pair the specification does not define
    /// is malformed. 16-bit samples, interlacing and methods other than 0
    /// are valid PNG that this decoder does not implement.
    fn parse(width: u32, height: u32, fields: &[u8]) -> Result<Self, DecodeError> {
        let &[bit_depth, color_type, compression, filter, interlace] = fields else {
            return Err(DecodeError::Malformed);
        };
        let defined = match color_type {
            PNG_GREYSCALE => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
            PNG_PALETTE => matches!(bit_depth, 1 | 2 | 4 | 8),
            PNG_RGB | PNG_GREYSCALE_ALPHA | PNG_RGBA => matches!(bit_depth, 8 | 16),
            _ => false,
        };
        if !defined {
            return Err(DecodeError::Malformed);
        }
        if bit_depth == 16 || compression != 0 || filter != 0 || interlace != 0 {
            return Err(DecodeError::Unsupported);
        }
        Ok(Self {
            width: width as usize,
            height: height as usize,
            bit_depth,
            color_type,
        })
    }

    fn channels(self) -> usize {
        match self.color_type {
            PNG_RGB => 3,
            PNG_GREYSCALE_ALPHA => 2,
            PNG_RGBA => 4,
            _ => 1,
        }
    }
}

/// Decodes non-interlaced PNG in every colour type and bit depth the
/// specification defines up to eight bits per sample: greyscale at 1, 2, 4
/// or 8 bits, palette at 1, 2, 4 or 8 bits, and 8-bit RGB, greyscale with
/// alpha and RGBA. `tRNS` supplies palette alpha or the one transparent
/// greyscale or RGB colour. Transparent pixels are composited over white
/// while converting to the framebuffer's RGB565 format.
pub fn decode_png(bytes: &[u8]) -> Result<DecodedImage, DecodeError> {
    const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() > MAX_IMAGE_COMPRESSED_BYTES {
        return Err(DecodeError::TooLarge);
    }
    if !bytes.starts_with(SIGNATURE) {
        return Err(DecodeError::Unsupported);
    }
    let mut offset = 8usize;
    let mut header: Option<PngHeader> = None;
    // RGBA per entry. Entries `tRNS` does not reach stay opaque.
    let mut palette = [[0u8, 0, 0, 255]; 256];
    let mut palette_len = None;
    let mut transparency_seen = false;
    let mut transparent: Option<[u16; 3]> = None;
    let mut compressed = Vec::new();
    let mut ended = false;
    while offset < bytes.len() {
        if offset + 12 > bytes.len() {
            return Err(DecodeError::Malformed);
        }
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let data_start = offset + 8;
        let data_end = data_start
            .checked_add(length)
            .ok_or(DecodeError::Malformed)?;
        let next = data_end.checked_add(4).ok_or(DecodeError::Malformed)?;
        if next > bytes.len() {
            return Err(DecodeError::Malformed);
        }
        let expected_crc = u32::from_be_bytes(bytes[data_end..next].try_into().unwrap());
        let kind_array: &[u8; 4] = kind.try_into().unwrap();
        let data = &bytes[data_start..data_end];
        if png_chunk_crc(kind_array, data) != expected_crc {
            return Err(DecodeError::Malformed);
        }
        offset = next;
        match kind {
            b"IHDR" if header.is_none() && length == 13 => {
                let width = u32::from_be_bytes(data[0..4].try_into().unwrap());
                let height = u32::from_be_bytes(data[4..8].try_into().unwrap());
                if !matches!(
                    dimensions(Format::Png, width, height),
                    Inspection::Dimensions(_)
                ) {
                    return Err(DecodeError::TooLarge);
                }
                header = Some(PngHeader::parse(width, height, &data[8..])?);
            }
            b"IHDR" => return Err(DecodeError::Malformed),
            // PLTE and tRNS only count before the image data. Late copies
            // are ignored, which leaves a palette image without its palette
            // and so fails it below.
            b"PLTE" if compressed.is_empty() => {
                let header = header.ok_or(DecodeError::Malformed)?;
                // Greyscale may not carry a palette, and truecolour only
                // suggests one for displays with fewer colours. Neither
                // changes the pixels, so both are ignored rather than
                // failing an image that is otherwise fine.
                if header.color_type != PNG_PALETTE {
                    continue;
                }
                if palette_len.is_some()
                    || transparency_seen
                    || length == 0
                    || length % 3 != 0
                    || length / 3 > 1 << header.bit_depth
                {
                    return Err(DecodeError::Malformed);
                }
                for (entry, rgb) in palette.iter_mut().zip(data.chunks_exact(3)) {
                    entry[..3].copy_from_slice(rgb);
                }
                palette_len = Some(length / 3);
            }
            b"tRNS" if compressed.is_empty() => {
                let header = header.ok_or(DecodeError::Malformed)?;
                if transparency_seen {
                    return Err(DecodeError::Malformed);
                }
                transparency_seen = true;
                let sample = |index: usize| u16::from_be_bytes([data[index], data[index + 1]]);
                match header.color_type {
                    PNG_PALETTE => {
                        let entries = palette_len.ok_or(DecodeError::Malformed)?;
                        if length > entries {
                            return Err(DecodeError::Malformed);
                        }
                        for (entry, alpha) in palette.iter_mut().zip(data) {
                            entry[3] = *alpha;
                        }
                    }
                    PNG_GREYSCALE if length == 2 => transparent = Some([sample(0); 3]),
                    PNG_RGB if length == 6 => {
                        transparent = Some([sample(0), sample(2), sample(4)]);
                    }
                    PNG_GREYSCALE | PNG_RGB => return Err(DecodeError::Malformed),
                    // Colour types with an alpha channel may not carry
                    // tRNS; like a stray PLTE it changes nothing.
                    _ => {}
                }
            }
            b"IDAT" if header.is_some() && !ended => {
                compressed
                    .try_reserve(length)
                    .map_err(|_| DecodeError::OutOfMemory)?;
                compressed.extend_from_slice(data);
                if compressed.len() > MAX_IMAGE_COMPRESSED_BYTES {
                    return Err(DecodeError::TooLarge);
                }
            }
            b"IEND" if length == 0 => {
                ended = true;
                break;
            }
            _ => {}
        }
    }
    let header = header.ok_or(DecodeError::Malformed)?;
    if !ended || compressed.is_empty() {
        return Err(DecodeError::Malformed);
    }
    if header.color_type == PNG_PALETTE && palette_len.is_none() {
        return Err(DecodeError::Malformed);
    }
    let palette_len = palette_len.unwrap_or(0);
    let (width, height) = (header.width, header.height);
    let bits_per_pixel = header.channels() * header.bit_depth as usize;
    let stride = width
        .checked_mul(bits_per_pixel)
        .ok_or(DecodeError::TooLarge)?
        .div_ceil(8);
    // Filters look back one whole pixel, or one byte when a pixel is
    // smaller than a byte.
    let filter_offset = (bits_per_pixel / 8).max(1);
    let raw_size = height
        .checked_mul(stride.checked_add(1).ok_or(DecodeError::TooLarge)?)
        .ok_or(DecodeError::TooLarge)?;
    if raw_size > MAX_IMAGE_DECODE_WORK_BYTES {
        return Err(DecodeError::TooLarge);
    }
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&compressed, raw_size)
        .map_err(|_| DecodeError::Malformed)?;
    if raw.len() != raw_size {
        return Err(DecodeError::Malformed);
    }
    let pixel_count = width.checked_mul(height).ok_or(DecodeError::TooLarge)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(pixel_count)
        .map_err(|_| DecodeError::OutOfMemory)?;
    let mut previous = alloc::vec![0u8; stride];
    let mut current = alloc::vec![0u8; stride];
    for row in 0..height {
        let source = row * (stride + 1);
        let filter = raw[source];
        for index in 0..stride {
            let byte = raw[source + 1 + index];
            let left = if index >= filter_offset {
                current[index - filter_offset]
            } else {
                0
            };
            let up = previous[index];
            let upper_left = if index >= filter_offset {
                previous[index - filter_offset]
            } else {
                0
            };
            current[index] = match filter {
                0 => byte,
                1 => byte.wrapping_add(left),
                2 => byte.wrapping_add(up),
                3 => byte.wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => byte.wrapping_add(paeth(left, up, upper_left)),
                _ => return Err(DecodeError::Malformed),
            };
        }
        for column in 0..width {
            let (red, green, blue, alpha) = match header.color_type {
                PNG_GREYSCALE => {
                    let value = packed_sample(&current, column, header.bit_depth);
                    let level = scale_sample(value, header.bit_depth);
                    // tRNS names the sample as stored, not scaled to 8 bits.
                    let alpha = if transparent == Some([value as u16; 3]) {
                        0
                    } else {
                        255
                    };
                    (level, level, level, alpha)
                }
                PNG_RGB => {
                    let pixel = &current[column * 3..column * 3 + 3];
                    let rgb = [pixel[0] as u16, pixel[1] as u16, pixel[2] as u16];
                    let alpha = if transparent == Some(rgb) { 0 } else { 255 };
                    (pixel[0], pixel[1], pixel[2], alpha)
                }
                PNG_PALETTE => {
                    let index = packed_sample(&current, column, header.bit_depth) as usize;
                    if index >= palette_len {
                        return Err(DecodeError::Malformed);
                    }
                    let [red, green, blue, alpha] = palette[index];
                    (red, green, blue, alpha)
                }
                PNG_GREYSCALE_ALPHA => {
                    let pixel = &current[column * 2..column * 2 + 2];
                    (pixel[0], pixel[0], pixel[0], pixel[1])
                }
                _ => {
                    let pixel = &current[column * 4..column * 4 + 4];
                    (pixel[0], pixel[1], pixel[2], pixel[3])
                }
            };
            pixels.push(rgb565_over_white(red, green, blue, alpha));
        }
        core::mem::swap(&mut previous, &mut current);
    }
    Ok(DecodedImage {
        width: width as u16,
        height: height as u16,
        pixels,
    })
}

/// Sample `index` of a row packed at `bit_depth` bits per sample, most
/// significant bit first as PNG stores them.
fn packed_sample(row: &[u8], index: usize, bit_depth: u8) -> u8 {
    let depth = bit_depth as usize;
    let bit = index * depth;
    let shift = 8 - depth - bit % 8;
    (row[bit / 8] >> shift) & (0xff >> (8 - depth))
}

/// Stretches a greyscale sample to 0..=255. Exact for 1, 2, 4 and 8 bits,
/// whose maxima all divide 255.
fn scale_sample(value: u8, bit_depth: u8) -> u8 {
    (value as u16 * 255 / ((1u16 << bit_depth) - 1)) as u8
}

fn rgb565_over_white(red: u8, green: u8, blue: u8, alpha: u8) -> u16 {
    let alpha = alpha as u16;
    let blend =
        |component: u8| -> u16 { (component as u16 * alpha + 255 * (255 - alpha) + 127) / 255 };
    ((blend(red) & 0xf8) << 8) | ((blend(green) & 0xfc) << 3) | (blend(blue) >> 3)
}

fn paeth(left: u8, up: u8, upper_left: u8) -> u8 {
    let estimate = left as i16 + up as i16 - upper_left as i16;
    let left_distance = (estimate - left as i16).unsigned_abs();
    let up_distance = (estimate - up as i16).unsigned_abs();
    let diagonal_distance = (estimate - upper_left as i16).unsigned_abs();
    if left_distance <= up_distance && left_distance <= diagonal_distance {
        left
    } else if up_distance <= diagonal_distance {
        up
    } else {
        upper_left
    }
}

pub fn inspect(bytes: &[u8]) -> Inspection {
    const PNG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < PNG.len() && PNG.starts_with(bytes) {
        return Inspection::NeedMore;
    }
    if bytes.starts_with(PNG) {
        if bytes.len() < 24 {
            return Inspection::NeedMore;
        }
        if &bytes[12..16] != b"IHDR" {
            return Inspection::Malformed;
        }
        return dimensions(
            Format::Png,
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        );
    }
    if bytes.len() < 4 && b"RIFF".starts_with(bytes) {
        return Inspection::NeedMore;
    }
    if bytes.starts_with(b"RIFF") {
        return inspect_webp(bytes);
    }
    if bytes.len() < 2 {
        return Inspection::NeedMore;
    }
    if bytes[..2] != [0xff, 0xd8] {
        return Inspection::Unsupported;
    }
    inspect_jpeg(bytes)
}

fn inspect_webp(bytes: &[u8]) -> Inspection {
    if bytes.len() < 12 {
        return Inspection::NeedMore;
    }
    if &bytes[8..12] != b"WEBP" {
        return Inspection::Unsupported;
    }
    let declared = match usize::try_from(u32::from_le_bytes(bytes[4..8].try_into().unwrap()))
        .ok()
        .and_then(|size| size.checked_add(8))
    {
        Some(size) if size >= 12 => size,
        _ => return Inspection::Malformed,
    };
    let mut offset = 12usize;
    while offset < declared {
        if offset + 8 > bytes.len() {
            return if bytes.len() >= declared {
                Inspection::Malformed
            } else {
                Inspection::NeedMore
            };
        }
        let kind = &bytes[offset..offset + 4];
        let length = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let data_start = offset + 8;
        let data_end = match data_start.checked_add(length) {
            Some(end) if end <= declared => end,
            _ => return Inspection::Malformed,
        };
        if data_end > bytes.len() {
            return Inspection::NeedMore;
        }
        let data = &bytes[data_start..data_end];
        match kind {
            b"VP8X" if data.len() >= 10 => {
                if data[0] & 0x02 != 0 {
                    return Inspection::Unsupported;
                }
                let width = 1 + u32::from_le_bytes([data[4], data[5], data[6], 0]);
                let height = 1 + u32::from_le_bytes([data[7], data[8], data[9], 0]);
                return dimensions(Format::WebP, width, height);
            }
            b"VP8 " if data.len() >= 10 => {
                if data[3..6] != [0x9d, 0x01, 0x2a] {
                    return Inspection::Malformed;
                }
                let width = u16::from_le_bytes([data[6], data[7]]) & 0x3fff;
                let height = u16::from_le_bytes([data[8], data[9]]) & 0x3fff;
                return dimensions(Format::WebP, u32::from(width), u32::from(height));
            }
            b"VP8L" if data.len() >= 5 => {
                if data[0] != 0x2f {
                    return Inspection::Malformed;
                }
                let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
                let width = (bits & 0x3fff) + 1;
                let height = ((bits >> 14) & 0x3fff) + 1;
                return dimensions(Format::WebP, width, height);
            }
            b"ANIM" | b"ANMF" => return Inspection::Unsupported,
            _ => {}
        }
        offset = match data_end.checked_add(length & 1) {
            Some(next) if next <= declared => next,
            _ => return Inspection::Malformed,
        };
    }
    if bytes.len() < declared {
        Inspection::NeedMore
    } else {
        Inspection::Malformed
    }
}

fn inspect_jpeg(bytes: &[u8]) -> Inspection {
    let mut offset = 2usize;
    loop {
        if offset >= bytes.len() {
            return Inspection::NeedMore;
        }
        if bytes[offset] != 0xff {
            return Inspection::Malformed;
        }
        while offset < bytes.len() && bytes[offset] == 0xff {
            offset += 1;
        }
        if offset >= bytes.len() {
            return Inspection::NeedMore;
        }
        let marker = bytes[offset];
        offset += 1;
        if marker == 0xd9 || marker == 0xda {
            return Inspection::Malformed;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        if offset + 2 > bytes.len() {
            return Inspection::NeedMore;
        }
        let length = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if length < 2 {
            return Inspection::Malformed;
        }
        let end = match offset.checked_add(length) {
            Some(end) => end,
            None => return Inspection::Malformed,
        };
        if end > bytes.len() {
            return Inspection::NeedMore;
        }
        if is_sof(marker) {
            if length < 8 {
                return Inspection::Malformed;
            }
            let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as u32;
            let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as u32;
            return dimensions(Format::Jpeg, width, height);
        }
        offset = end;
    }
}

fn is_sof(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn dimensions(format: Format, width: u32, height: u32) -> Inspection {
    let pixels = width.checked_mul(height);
    if width == 0 || height == 0 {
        Inspection::Malformed
    } else if width > MAX_IMAGE_WIDTH
        || height > MAX_IMAGE_HEIGHT
        || pixels.is_none_or(|pixels| pixels as usize > MAX_IMAGE_PIXELS)
    {
        Inspection::TooLarge
    } else {
        Inspection::Dimensions(Dimensions {
            format,
            width,
            height,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASELINE_JPEG: &str = concat!(
        "/9j/4AAQSkZJRgABAQAAAQABAAD/2wBDAAUDBAQEAwUEBAQFBQUGBwwIBwcHBw8LCwkMEQ8SEhEP",
        "ERETFhwXExQaFRERGCEYGh0dHx8fExciJCIeJBweHx7/2wBDAQUFBQcGBw4ICA4eFBEUHh4eHh4e",
        "Hh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh7/wAARCAAwAEAD",
        "ASIAAhEBAxEB/8QAGwAAAgMBAQEAAAAAAAAAAAAABAYABQcCAwj/xAAyEAACAQIEBQIEBAcAAAAAAA",
        "ABAgMABAURIWEGEjFBUQcTFCJx0RUWI8EyQlKBkbHw/8QAGwEAAgIDAQAAAAAAAAAAAAAABgcDBAEC",
        "BQj/xAAoEQABAwQBAwIHAAAAAAAAAAABAAIEAwURMRIGIUETUTJhgZGh4fD/2gAMAwEAAhEDEQA/",
        "AMait9qKit9qMit9qKit9qbFeuhqHJQcVvtRUVvtRkVvtRUVvtXFr10Uw5KDit9qbeBOCb7ia4co/",
        "wALZxaSXLJzDmy0VRmOY9M9dB9QDURW+1bl6Sm2/JdukAQSJLIJ8lyJfmzGZ7nlKa+Mh2pedcX+T",
        "abaa0b4yQ3OM8c57+3jAz2yQimFXLtKjxL0lwz4Q/heI3UdwNV+JKujaHT5VBGuWuuWuhrOLrDLm",
        "wvJLO8gaGeJuV0bqD+43719IVlXqelvLxV+iq86wIs2S5EvqRn5PKV1+g7UvejeqLhMkuiyncxgk",
        "E7GPn7HPnzhEMGU8P4nSySK32olYVVSzEKoGZJ6AUbFb7UvcQYssnNZ2bfp9JJB/NsNt+/06v24",
        "XBsdhc76LzfAqueey5fGlS+X24+a2XQ6fM24+3/Bqt4VdVdCGVhmCDmCKzqmLg7GFtLkWd5Ly2r",
        "/AMDN0jbPz2B/341oVoXOo5xFU7/H6RHGkljsFN0VvtV5w1id/gd009ky5OvLJG4JR/GY8jsfua8",
        "orfaqHjDiCPCIzZ2hV79hr3EIPc7+B/c9gYJ/pSKTqVZoc07BRPQuDaDObjpajecdXk1uUtLCO2",
        "kOnuPJ7mQy7DIa9OuY2pV9p5ZGkkZndyWZmOZYnqSaV/TviB8Rf8Kvm5rlELRSlhnIB2Plh13AO",
        "fTMvsVvtQ1Gt0O2NLYtPjneyfuclE1suTJLBUaVkfFuKxojYdayEvnlOynQD+j7/wCPNK1SpXdl",
        "SXyahe5J6LHbHphjVKlSpVdWE04Xxjc2HDpw9Iua6T5YJ2OYVDn1B6kdB2yy8arEjvLI0kjs7uS",
        "zMxzJJ6kmualZJJ2tnPc4AE6XUUkkUqSxO0ciMGVlORUjoQexraPTPileIYWsbxeXEoI+dmVfllQ",
        "EDm8A5kZjfMdwMVojDb27w2+hvrGd4LiFuaN16g/uOxB0I0NQVqXqNx5V63XB8KryGvI/vK//2Q=="
    );

    fn test_jpeg() -> Vec<u8> {
        let mut out = Vec::new();
        let mut value = 0u32;
        let mut bits = 0u8;
        for byte in BASELINE_JPEG.bytes().filter(|byte| *byte != b'=') {
            let digit = match byte {
                b'A'..=b'Z' => byte - b'A',
                b'a'..=b'z' => byte - b'a' + 26,
                b'0'..=b'9' => byte - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("bad base64 fixture"),
            };
            value = (value << 6) | u32::from(digit);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((value >> bits) as u8);
                value &= (1 << bits) - 1;
            }
        }
        out
    }

    fn png(
        width: u32,
        height: u32,
        bit_depth: u8,
        color_type: u8,
        extra: &[(&[u8; 4], &[u8])],
        rows: &[u8],
    ) -> Vec<u8> {
        fn chunk(target: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            target.extend_from_slice(&(data.len() as u32).to_be_bytes());
            target.extend_from_slice(kind);
            target.extend_from_slice(data);
            target.extend_from_slice(&png_chunk_crc(kind, data).to_be_bytes());
        }
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[bit_depth, color_type, 0, 0, 0]);
        chunk(&mut bytes, b"IHDR", &ihdr);
        for (kind, data) in extra {
            chunk(&mut bytes, kind, data);
        }
        chunk(
            &mut bytes,
            b"IDAT",
            &miniz_oxide::deflate::compress_to_vec_zlib(rows, 6),
        );
        chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    fn grey(level: u8) -> u16 {
        rgb565_over_white(level, level, level, 255)
    }

    fn webp_riff(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        for (kind, data) in chunks {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&(data.len() as u32).to_le_bytes());
            body.extend_from_slice(data);
            if data.len() & 1 != 0 {
                body.push(0);
            }
        }
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn png_dimensions_wait_for_the_whole_ihdr_prefix() {
        let bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x20\0\0\0\x10";
        for end in 0..24 {
            assert_eq!(inspect(&bytes[..end]), Inspection::NeedMore, "prefix {end}");
        }
        assert_eq!(
            inspect(bytes),
            Inspection::Dimensions(Dimensions {
                format: Format::Png,
                width: 32,
                height: 16
            })
        );
    }

    #[test]
    fn jpeg_skips_variable_length_metadata() {
        let bytes = b"\xff\xd8\xff\xe1\0\x08abcdef\xff\xc0\0\x0b\x08\0\x10\0\x20\x03\0\0\0";
        assert_eq!(
            inspect(bytes),
            Inspection::Dimensions(Dimensions {
                format: Format::Jpeg,
                width: 32,
                height: 16
            })
        );
        assert_eq!(inspect(&bytes[..10]), Inspection::NeedMore);
    }

    #[test]
    fn dimensions_are_bounded_before_decode() {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend_from_slice(&(MAX_IMAGE_WIDTH + 1).to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(inspect(&bytes), Inspection::TooLarge);
    }

    #[test]
    fn inspects_and_decodes_static_lossless_webp() {
        let rgba = [255, 0, 0, 255, 0, 0, 255, 128];
        let bytes = webpkit::encode_lossless_rgba(2, 1, &rgba).unwrap();
        assert_eq!(
            inspect(&bytes),
            Inspection::Dimensions(Dimensions {
                format: Format::WebP,
                width: 2,
                height: 1,
            })
        );
        let image = decode(&bytes).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.pixels, [0xf800, rgb565_over_white(0, 0, 255, 128)]);
    }

    #[test]
    fn decodes_lossy_webp_with_alpha_container() {
        let mut rgba = Vec::new();
        for y in 0..16u8 {
            for x in 0..16u8 {
                rgba.extend_from_slice(&[
                    x.saturating_mul(16),
                    y.saturating_mul(16),
                    128,
                    x.saturating_add(y).saturating_mul(8),
                ]);
            }
        }
        let bytes = webpkit::encode_lossy_rgba(16, 16, &rgba, 80).unwrap();
        assert_eq!(
            inspect(&bytes),
            Inspection::Dimensions(Dimensions {
                format: Format::WebP,
                width: 16,
                height: 16,
            })
        );
        let image = decode(&bytes).unwrap();
        assert_eq!(
            (image.width, image.height, image.pixels.len()),
            (16, 16, 256)
        );
    }

    #[test]
    fn animated_webp_is_rejected_before_decode() {
        let vp8x = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let bytes = webp_riff(&[(b"VP8X", &vp8x), (b"ANIM", &[0; 6])]);
        assert_eq!(inspect(&bytes), Inspection::Unsupported);
        assert!(matches!(decode(&bytes), Err(DecodeError::Unsupported)));
    }

    #[test]
    fn webp_work_limit_and_truncation_are_local_errors() {
        // Inside the generic 2-Mpixel image limit but above WebP's
        // 4-MiB RGBA8 work limit.
        let bits = (1100u32 - 1) | ((1000u32 - 1) << 14);
        let mut vp8l = vec![0x2f];
        vp8l.extend_from_slice(&bits.to_le_bytes());
        let too_large = webp_riff(&[(b"VP8L", &vp8l)]);
        assert!(matches!(decode(&too_large), Err(DecodeError::TooLarge)));

        let complete = webpkit::encode_lossless_rgba(1, 1, &[0, 0, 0, 255]).unwrap();
        assert_eq!(inspect(&complete[..12]), Inspection::NeedMore);
        assert!(matches!(
            decode(&complete[..12]),
            Err(DecodeError::Malformed)
        ));
    }

    #[test]
    fn decodes_rgb_and_rgba_over_white_to_rgb565() {
        let rgb = decode_png(&png(2, 1, 8, 2, &[], &[0, 255, 0, 0, 0, 255, 0])).unwrap();
        assert_eq!((rgb.width, rgb.height), (2, 1));
        assert_eq!(rgb.pixels, [0xf800, 0x07e0]);

        let rgba = decode_png(&png(1, 1, 8, 6, &[], &[0, 0, 0, 255, 0])).unwrap();
        assert_eq!(rgba.pixels, [0xffff]);
    }

    #[test]
    fn decodes_every_greyscale_depth_up_to_eight_bits() {
        // The 1-bit row's low nibble is padding and must not leak in.
        let cases: [(u8, &[u8], [u8; 4]); 4] = [
            (1, &[0, 0b1010_1111], [255, 0, 255, 0]),
            (2, &[0, 0b0001_1011], [0, 85, 170, 255]),
            (4, &[0, 0x0f, 0x85], [0, 255, 136, 85]),
            (8, &[0, 0, 128, 200, 255], [0, 128, 200, 255]),
        ];
        for (depth, row, levels) in cases {
            let image = decode_png(&png(4, 1, depth, 0, &[], row)).unwrap();
            assert_eq!(image.pixels, levels.map(grey), "depth {depth}");
        }
    }

    #[test]
    fn decodes_packed_palette_across_bytes_with_palette_alpha() {
        // Nine 1-bit pixels span two bytes. The second row uses the Sub
        // filter, which looks back one byte when a pixel is smaller: 0x80
        // plus 0x80 wraps, clearing the ninth pixel.
        let plte: &[u8] = &[0, 0, 0, 255, 0, 0];
        let rows = [0, 0b1000_0000, 0b1000_0000, 1, 0b1000_0000, 0b1000_0000];
        let image =
            decode_png(&png(9, 2, 1, 3, &[(b"PLTE", plte), (b"tRNS", &[0])], &rows)).unwrap();
        let (red, white) = (0xf800, 0xffff);
        let mut expected = [white; 18];
        expected[0] = red;
        expected[8] = red;
        expected[9] = red;
        assert_eq!(image.pixels, expected);

        let plte: &[u8] = &[0, 0, 255, 0, 255, 0];
        let cases: [(u8, &[u8]); 3] = [(2, &[0, 0b0100_0000]), (4, &[0, 0x10]), (8, &[0, 1, 0])];
        for (depth, row) in cases {
            let image = decode_png(&png(2, 1, depth, 3, &[(b"PLTE", plte)], row)).unwrap();
            assert_eq!(image.pixels, [0x07e0, 0x001f], "depth {depth}");
        }
    }

    #[test]
    fn decodes_greyscale_alpha_and_single_transparent_colours() {
        // Sub over two-byte pixels: the second pixel adds the first, and its
        // alpha wraps to zero.
        let image = decode_png(&png(2, 1, 8, 4, &[], &[1, 100, 255, 20, 1])).unwrap();
        assert_eq!(image.pixels, [grey(100), 0xffff]);

        let image = decode_png(&png(3, 1, 8, 0, &[(b"tRNS", &[0, 7])], &[0, 7, 8, 0])).unwrap();
        assert_eq!(image.pixels, [0xffff, grey(8), grey(0)]);

        // The key is the stored 2-bit sample 2, not its scaled level 170.
        let image = decode_png(&png(2, 1, 2, 0, &[(b"tRNS", &[0, 2])], &[0, 0b1001_0000])).unwrap();
        assert_eq!(image.pixels, [0xffff, grey(85)]);

        let key: &[u8] = &[0, 255, 0, 0, 0, 0];
        let image = decode_png(&png(
            2,
            1,
            8,
            2,
            &[(b"tRNS", key)],
            &[0, 255, 0, 0, 255, 0, 1],
        ))
        .unwrap();
        assert_eq!(image.pixels, [0xffff, 0xf800]);
    }

    #[test]
    fn ignores_palette_and_transparency_that_change_nothing() {
        let image = decode_png(&png(1, 1, 8, 0, &[(b"PLTE", &[1, 2, 3])], &[0, 9])).unwrap();
        assert_eq!(image.pixels, [grey(9)]);
        let image = decode_png(&png(1, 1, 8, 6, &[(b"tRNS", &[0])], &[0, 0, 0, 0, 255])).unwrap();
        assert_eq!(image.pixels, [0]);
    }

    #[test]
    fn rejects_undefined_depths_and_inconsistent_palettes() {
        let plte: &[u8] = &[0, 0, 0];
        let cases: [(&str, Vec<u8>); 9] = [
            ("4-bit RGB", png(1, 1, 4, 2, &[], &[0, 0])),
            ("colour type 1", png(1, 1, 8, 1, &[], &[0, 0])),
            ("16-bit palette", png(1, 1, 16, 3, &[], &[0, 0, 0])),
            ("palette without PLTE", png(1, 1, 1, 3, &[], &[0, 0])),
            (
                "three entries at 1 bit",
                png(1, 1, 1, 3, &[(b"PLTE", &[0; 9])], &[0, 0]),
            ),
            (
                "index past palette",
                png(1, 1, 8, 3, &[(b"PLTE", plte)], &[0, 1]),
            ),
            (
                "tRNS longer than palette",
                png(1, 1, 8, 3, &[(b"PLTE", plte), (b"tRNS", &[0, 0])], &[0, 0]),
            ),
            (
                "tRNS before PLTE",
                png(1, 1, 8, 3, &[(b"tRNS", &[0]), (b"PLTE", plte)], &[0, 0]),
            ),
            (
                "short greyscale tRNS",
                png(1, 1, 8, 0, &[(b"tRNS", &[0])], &[0, 0]),
            ),
        ];
        for (name, bytes) in cases {
            assert_eq!(
                decode_png(&bytes).unwrap_err(),
                DecodeError::Malformed,
                "{name}"
            );
        }
    }

    #[test]
    fn rejects_sixteen_bits_interlace_and_bad_filter() {
        for color_type in [0, 2, 4, 6] {
            assert_eq!(
                decode_png(&png(1, 1, 16, color_type, &[], &[0; 9])).unwrap_err(),
                DecodeError::Unsupported,
                "colour type {color_type}"
            );
        }
        let mut interlaced = png(1, 1, 8, 2, &[], &[0, 0, 0, 0]);
        interlaced[28] = 1;
        let crc = png_chunk_crc(b"IHDR", &interlaced[16..29]);
        interlaced[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(
            decode_png(&interlaced).unwrap_err(),
            DecodeError::Unsupported
        );
        assert_eq!(
            decode_png(&png(1, 1, 8, 2, &[], &[5, 0, 0, 0])).unwrap_err(),
            DecodeError::Malformed
        );
    }

    #[test]
    fn rejects_a_png_chunk_with_a_bad_crc() {
        let mut bytes = png(1, 1, 8, 2, &[], &[0, 1, 2, 3]);
        bytes[29] ^= 1;
        assert_eq!(decode_png(&bytes).unwrap_err(), DecodeError::Malformed);
    }

    #[test]
    fn decodes_baseline_jpeg_to_rgb565() {
        let image = decode(&test_jpeg()).unwrap();
        assert_eq!((image.width, image.height), (64, 48));
        assert_eq!(image.pixels.len(), 64 * 48);
        assert!(image.pixels.windows(2).any(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn rejects_progressive_jpeg_before_decode() {
        let mut bytes = test_jpeg();
        let sof = bytes
            .windows(2)
            .position(|pair| pair == [0xff, 0xc0])
            .unwrap();
        bytes[sof + 1] = 0xc2;
        assert!(matches!(decode_jpeg(&bytes), Err(DecodeError::Unsupported)));
    }
}
