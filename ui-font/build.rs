use std::env;
use std::fs;
use std::path::PathBuf;

const SOURCE: &str = "data/tab5-ui-fonts.bin";

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    let data = fs::read(SOURCE).expect("read generated UI font blob");
    assert_eq!(&data[..4], b"T5A4");
    assert_eq!(u16_at(&data, 4), 1);
    assert_eq!(u16_at(&data, 6), 32);

    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let compressed = tab5_font_codec::encode_container(&data);
    fs::write(out.join("tab5-ui-fonts.lz4"), &compressed).unwrap();

    let metadata = format!(
        "pub const STORAGE_BYTES: usize = {};\n\
         pub const STRIKE_COUNT: usize = {};\n\
         pub const GLYPH_COUNT: usize = {};\n\
         const STRIKES_OFFSET: usize = {};\n\
         const GLYPHS_OFFSET: usize = {};\n\
         const BITMAPS_OFFSET: usize = {};\n\
         pub const TOTAL_BYTES: usize = {};\n\
         pub const COMPRESSED_BYTES: usize = {};\n\
         pub const CRC32: u32 = 0x{:08x};\n",
        data.len(),
        u16_at(&data, 8),
        u16_at(&data, 10),
        u32_at(&data, 12),
        u32_at(&data, 16),
        u32_at(&data, 20),
        u32_at(&data, 24),
        compressed.len(),
        u32_at(&data, 28),
    );
    fs::write(out.join("font_meta.rs"), metadata).unwrap();
}
