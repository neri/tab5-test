//! ESP32-P4 SDHOST controller bring-up, SD card activation and DMA block I/O
//! (stages 1-3 of `SD_CARD_PLAN.md`).
//!
//! Targets the Tab5's microSD slot: SDIO1, IOMUX-routed (bypasses the GPIO
//! matrix), 4-bit capable, GPIO39..44 = D0,D1,D2,D3,CLK,CMD (confirmed from
//! the Tab5 schematic's `TF_CARD_SOCKET`/`SDIO1_*` nets). Card VDD is tied
//! directly to `SOC_3.3V` with no power-switch GPIO, and the socket's
//! `Detect` pin is not wired to any SoC GPIO, so card presence is inferred
//! from command timeouts rather than a card-detect line.
//!
//! Register field layout is taken from ESP-IDF v5.5.3
//! (`components/soc/esp32p4/register/hw_ver1/soc/sdmmc_reg.h`,
//! `hp_sys_clkrst_reg.h`, `lp_clkrst_reg.h`) and the DW-MMC-derived command
//! sequence mirrors `components/esp_driver_sdmmc/src/sdmmc_host.c`. Bus
//! initialization and card identification are plain polled commands with no
//! data phase.
//!
//! Block reads (stage 2) use the SDHOST's internal DMA (IDMAC), not CPU/APB
//! polling of `SDHOST_BUFFIFO_REG`: on real hardware, repeated APB reads of
//! that register never advanced past the first word (`STATUS.FIFO_COUNT`
//! kept climbing — real data was arriving from the card — while the word
//! value read back stayed constant, with both a fixed address and an
//! incrementing address into the FIFO's documented 512-word window). Since
//! ESP-IDF's own driver only ever uses DMA for data phases and never
//! exercises that CPU/APB path, this may be a genuine gap in this specific
//! peripheral revision rather than a driver bug; IDMAC is the only
//! data-phase path that is actually exercised by production ESP-IDF code.
//!
//! IDMAC descriptors and destination buffers are internal SRAM, which on
//! ESP32-P4 (`SOC_CACHE_INTERNAL_MEM_VIA_L1CACHE`) sits behind the L1/L2
//! cache like PSRAM does, so both need the same
//! `Cache_WriteBack_Invalidate_Addr` ROM call `psram.rs` uses for its
//! framebuffer: writeback before handing a descriptor to the DMA engine (it
//! reads RAM directly, bypassing cache), invalidate before the CPU reads a
//! DMA-written buffer.
//!
//! Stage 3 extends the single-descriptor read into a chained-descriptor,
//! multi-block read/write (`read_blocks`/`write_blocks`, CMD18/CMD25 with
//! hardware auto-stop instead of a manual CMD12).

use core::mem::size_of;

use crate::uart;

const SDHOST: usize = 0x5008_3000;
const HP_SYS_CLKRST: usize = 0x500E_6000;
const LP_CLKRST: usize = 0x5011_1000;
const IO_MUX: usize = 0x500E_1000;

const CTRL: usize = SDHOST + 0x00;
const CLKDIV: usize = SDHOST + 0x08;
const CLKSRC: usize = SDHOST + 0x0C;
const CLKENA: usize = SDHOST + 0x10;
const TMOUT: usize = SDHOST + 0x14;
const BLKSIZ: usize = SDHOST + 0x1C;
const BYTCNT: usize = SDHOST + 0x20;
const CMDARG: usize = SDHOST + 0x28;
const CMD: usize = SDHOST + 0x2C;
const RESP0: usize = SDHOST + 0x30;
const RINTSTS: usize = SDHOST + 0x44;
const STATUS: usize = SDHOST + 0x48;
const BMOD: usize = SDHOST + 0x80;
const PLDMND: usize = SDHOST + 0x84;
const DBADDR: usize = SDHOST + 0x88;
const IDSTS: usize = SDHOST + 0x8C;
const CARDTHRCTL: usize = SDHOST + 0x100;

const SOC_CLK_CTRL1: usize = HP_SYS_CLKRST + 0x18;
const PERI_CLK_CTRL01: usize = HP_SYS_CLKRST + 0x34;
const PERI_CLK_CTRL02: usize = HP_SYS_CLKRST + 0x38;
const HP_SDMMC_EMAC_RST_CTRL: usize = LP_CLKRST + 0x4C;

const PIN_D0: u32 = 39;
const PIN_D1: u32 = 40;
const PIN_D2: u32 = 41;
const PIN_D3: u32 = 42;
const PIN_CLK: u32 = 43;
const PIN_CMD: u32 = 44;
const SDMMC_IOMUX_FUNC: u32 = 0;

const CMD_RESPONSE_EXPECT: u32 = 1 << 6;
const CMD_RESPONSE_LONG: u32 = 1 << 7;
const CMD_CHECK_RESPONSE_CRC: u32 = 1 << 8;
const CMD_DATA_EXPECTED: u32 = 1 << 9;
const CMD_READ_WRITE: u32 = 1 << 10; // 0: read from card, 1: write to card
const CMD_SEND_AUTO_STOP: u32 = 1 << 12; // hardware sends CMD12 after the transfer itself
const CMD_WAIT_PRVDATA_COMPLETE: u32 = 1 << 13;
const CMD_SEND_INITIALIZATION: u32 = 1 << 15;
const CMD_UPDATE_CLOCK_REGISTERS_ONLY: u32 = 1 << 21;
const CMD_USE_HOLD_REG: u32 = 1 << 29;
const CMD_START: u32 = 1 << 31;

const RINT_RESPONSE_ERROR: u32 = 1 << 1;
const RINT_COMMAND_DONE: u32 = 1 << 2;
const RINT_DATA_TRANSFER_OVER: u32 = 1 << 3;
const RINT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
const RINT_DATA_CRC_ERROR: u32 = 1 << 7;
const RINT_RESPONSE_TIMEOUT: u32 = 1 << 8;
const RINT_DATA_READ_TIMEOUT: u32 = 1 << 9;
const RINT_FIFO_ERROR: u32 = 1 << 11;
const RINT_HARDWARE_LOCKED_ERROR: u32 = 1 << 12;
const RINT_START_BIT_ERROR: u32 = 1 << 13;
const RINT_ALL: u32 = 0xFFFF;

const BLOCK_BYTES: usize = 512;

const OCR_HIGH_CAPACITY: u32 = 1 << 30;
const OCR_BUSY: u32 = 1 << 31;
const OCR_VOLTAGE_WINDOW: u32 = 0x00FF_8000; // ~2.7-3.6V

// `SDHOST_CTRL_REG` bits not named in the register header but present in
// `sdmmc_struct.h`'s bitfield layout (they sit between/after the named
// reset and enable bits).
const CTRL_INT_ENABLE: u32 = 1 << 4;
const CTRL_DMA_ENABLE: u32 = 1 << 5;
const CTRL_USE_INTERNAL_DMA: u32 = 1 << 25;

const BMOD_SWR: u32 = 1 << 0; // software reset, self-clearing
const BMOD_FB: u32 = 1 << 1; // fixed burst
const BMOD_DE: u32 = 1 << 7; // IDMAC enable

const CARDTHRCTL_CARDRDTHREN: u32 = 1 << 0;

/// `IDSTS.RI`/`FBE`/`DU`/`CES` never latch on this hardware (see `read_block`),
/// so this is only used to clear any stray bits, never to check status.
const IDSTS_ALL: u32 = 0x1FF;

/// Descriptor control word (des0) bits, matching ESP-IDF's `sdmmc_desc_t`.
const DESC_LAST: u32 = 1 << 2;
const DESC_FIRST: u32 = 1 << 3;
const DESC_CHAINED: u32 = 1 << 4;
const DESC_OWNED_BY_IDMAC: u32 = 1 << 31;

const ROM_CACHE_WRITEBACK_INVALIDATE_ADDR: usize = 0x4FC0_03FC;
const CACHE_MAP_L1_DCACHE: u32 = 1 << 4;
const CACHE_MAP_L2_CACHE: u32 = 1 << 5;

/// One IDMAC descriptor. Layout and 64-byte size (the extra `reserved` words
/// are cache-line padding on P4) match ESP-IDF's `sdmmc_desc_t` exactly.
/// `read_block` builds a single one, non-chained, since one block fits in a
/// descriptor's 4096-byte limit; `read_blocks`/`write_blocks` chain several
/// (`second_address_chained`/`next_desc_ptr`) for larger transfers.
#[derive(Clone, Copy)]
#[repr(C, align(64))]
struct Descriptor {
    control: u32,
    buffer1_size: u32,
    buffer1_ptr: u32,
    next_or_buffer2_ptr: u32,
    reserved: [u32; 12],
}

impl Descriptor {
    const fn zeroed() -> Self {
        Descriptor {
            control: 0,
            buffer1_size: 0,
            buffer1_ptr: 0,
            next_or_buffer2_ptr: 0,
            reserved: [0; 12],
        }
    }
}

/// Matches `SDMMC_DMA_MAX_BUF_LEN`: the largest single buffer a descriptor's
/// 13-bit `buffer1_size` field is used for (ESP-IDF caps it here too, well
/// under the field's 8191-byte range).
const DESC_MAX_BUFFER_BYTES: usize = 4096;
/// Blocks per call are capped by this many chained descriptors -- 8 * 4096
/// bytes = 64 KiB, comfortably more than any single shell command needs.
const MAX_DESCRIPTORS: usize = 8;

pub struct SdCard {
    pub rca: u16,
    pub high_capacity: bool,
    pub cid: [u32; 4],
    pub csd: [u32; 4],
    /// Approximate capacity in bytes, decoded from a CSD version 2.0
    /// (SDHC/SDXC) structure. `None` for CSD version 1.0 cards.
    pub capacity_bytes: Option<u64>,
}

/// Resets, clocks and activates the card in the Tab5's microSD slot,
/// logging progress and any failure reason over USB serial.
pub fn init() -> Option<SdCard> {
    enable_bus_clock();
    reset_peripheral();
    configure_pins();
    set_low_speed_clock_source(2);
    if !reset_controller() {
        uart::log(b"SDMMC: controller reset timed out\r\n");
        return None;
    }
    init_dma();
    // 160 MHz / 10 / (2*20) = 400 kHz identification clock.
    if !set_card_clock(10, 20) {
        uart::log(b"SDMMC: card clock setup failed (clock-update command not accepted)\r\n");
        dump_diagnostics();
        return None;
    }
    unsafe { write(TMOUT, (0xFFFFFFu32 << 8) | 0xFF) };

    if send_command(0, 0, CMD_SEND_INITIALIZATION).is_err() {
        uart::log(b"SDMMC: CMD0 (GO_IDLE_STATE) failed\r\n");
        dump_diagnostics();
        return None;
    }

    let mut high_speed_ok = true;
    match send_command(8, 0x1AA, CMD_RESPONSE_EXPECT | CMD_CHECK_RESPONSE_CRC) {
        Ok(resp) if resp[0] & 0xFF == 0xAA => {}
        Ok(resp) => {
            uart::log_hex(b"SDMMC: CMD8 unexpected echo=", resp[0]);
            high_speed_ok = false;
        }
        Err(_) => {
            uart::log(b"SDMMC: CMD8 (SEND_IF_COND) timed out; assuming SD ver1.x/MMC\r\n");
            dump_diagnostics();
            high_speed_ok = false;
        }
    }

    let acmd41_arg = OCR_VOLTAGE_WINDOW | if high_speed_ok { OCR_HIGH_CAPACITY } else { 0 };
    let mut ocr = 0u32;
    let mut ready = false;
    for _ in 0..1000 {
        if send_command(55, 0, CMD_RESPONSE_EXPECT | CMD_CHECK_RESPONSE_CRC).is_err() {
            uart::log(b"SDMMC: CMD55 (APP_CMD) failed\r\n");
            return None;
        }
        match send_command(41, acmd41_arg, CMD_RESPONSE_EXPECT) {
            Ok(resp) => {
                ocr = resp[0];
                if ocr & OCR_BUSY != 0 {
                    ready = true;
                    break;
                }
            }
            Err(_) => {
                uart::log(b"SDMMC: ACMD41 (SD_SEND_OP_COND) failed\r\n");
                return None;
            }
        }
        delay_us(1000);
    }
    if !ready {
        uart::log(b"SDMMC: card did not leave busy state (no card, or unsupported voltage)\r\n");
        return None;
    }
    let high_capacity = high_speed_ok && (ocr & OCR_HIGH_CAPACITY != 0);
    uart::log_hex(b"SDMMC: OCR=", ocr);

    let cid = match send_command(
        2,
        0,
        CMD_RESPONSE_EXPECT | CMD_RESPONSE_LONG | CMD_CHECK_RESPONSE_CRC,
    ) {
        Ok(resp) => resp,
        Err(_) => {
            uart::log(b"SDMMC: CMD2 (ALL_SEND_CID) failed\r\n");
            return None;
        }
    };

    let resp = match send_command(3, 0, CMD_RESPONSE_EXPECT | CMD_CHECK_RESPONSE_CRC) {
        Ok(resp) => resp,
        Err(_) => {
            uart::log(b"SDMMC: CMD3 (SEND_RELATIVE_ADDR) failed\r\n");
            return None;
        }
    };
    let rca = (resp[0] >> 16) as u16;
    let rca_arg = (rca as u32) << 16;

    let csd = match send_command(
        9,
        rca_arg,
        CMD_RESPONSE_EXPECT | CMD_RESPONSE_LONG | CMD_CHECK_RESPONSE_CRC,
    ) {
        Ok(resp) => resp,
        Err(_) => {
            uart::log(b"SDMMC: CMD9 (SEND_CSD) failed\r\n");
            return None;
        }
    };

    if send_command(7, rca_arg, CMD_RESPONSE_EXPECT | CMD_CHECK_RESPONSE_CRC).is_err() {
        uart::log(b"SDMMC: CMD7 (SELECT_CARD) failed\r\n");
        return None;
    }
    wait_data_not_busy();

    let capacity_bytes = decode_csd_v2_capacity(csd);

    uart::log(b"SDMMC: card activated\r\n");
    uart::log_hex(b"SDMMC: CID[3]=", cid[3]);
    uart::log_hex(b"SDMMC: CID[2]=", cid[2]);
    uart::log_hex(b"SDMMC: CID[1]=", cid[1]);
    uart::log_hex(b"SDMMC: CID[0]=", cid[0]);
    uart::log_hex(b"SDMMC: CSD[3]=", csd[3]);
    uart::log_hex(b"SDMMC: CSD[2]=", csd[2]);
    uart::log_hex(b"SDMMC: CSD[1]=", csd[1]);
    uart::log_hex(b"SDMMC: CSD[0]=", csd[0]);

    Some(SdCard {
        rca,
        high_capacity,
        cid,
        csd,
        capacity_bytes,
    })
}

/// Reads one 512-byte block via CMD17 (`READ_SINGLE_BLOCK`) using the
/// SDHOST's internal DMA (IDMAC) with a single one-shot descriptor. `lba` is
/// a block index; standard-capacity (CSD v1) cards address blocks by byte
/// offset on the wire, so `card.high_capacity` selects whether `lba` is sent
/// as-is or multiplied by the block size.
pub fn read_block(card: &SdCard, lba: u32, buffer: &mut [u8; BLOCK_BYTES]) -> bool {
    let address = if card.high_capacity {
        lba
    } else {
        lba.wrapping_mul(BLOCK_BYTES as u32)
    };

    // Push out (and drop) any stale dirty cache lines for the destination
    // buffer before DMA writes to the underlying RAM directly; otherwise a
    // later, unrelated cache eviction could flush old CPU-side data over
    // what DMA just wrote. Matches ESP-IDF's C2M `esp_cache_msync` on the
    // buffer before `sdmmc_host_dma_prepare`.
    cache_writeback_invalidate(buffer.as_ptr() as usize, BLOCK_BYTES);

    let mut descriptor = Descriptor {
        control: DESC_OWNED_BY_IDMAC | DESC_FIRST | DESC_LAST | DESC_CHAINED,
        buffer1_size: BLOCK_BYTES as u32,
        buffer1_ptr: buffer.as_mut_ptr() as u32,
        next_or_buffer2_ptr: 0,
        reserved: [0; 12],
    };
    let descriptor_address = &raw mut descriptor as usize;
    cache_writeback_invalidate(descriptor_address, size_of::<Descriptor>());

    unsafe {
        write(BLKSIZ, BLOCK_BYTES as u32);
        write(BYTCNT, BLOCK_BYTES as u32);
        // Card read threshold = block size. Undocumented in ESP-IDF (which
        // never touches this register), but this is a common DW-MMC pattern
        // for making the FIFO-to-RAM burst engine actually start.
        write(CARDTHRCTL, (BLOCK_BYTES as u32) << 16 | CARDTHRCTL_CARDRDTHREN);
        modify(CTRL, CTRL_USE_INTERNAL_DMA, CTRL_USE_INTERNAL_DMA);
        modify(BMOD, BMOD_DE | BMOD_FB, BMOD_DE | BMOD_FB);
        write(DBADDR, descriptor_address as u32);
        write(PLDMND, 1);
    }

    let flags =
        CMD_RESPONSE_EXPECT | CMD_CHECK_RESPONSE_CRC | CMD_DATA_EXPECTED | CMD_WAIT_PRVDATA_COMPLETE;
    if send_command(17, address, flags).is_err() {
        uart::log(b"SDMMC: CMD17 (READ_SINGLE_BLOCK) failed\r\n");
        return false;
    }

    // IDSTS.RI never latches on this hardware even though the transfer
    // genuinely completes (STATUS.FIFO_EMPTY goes back to 1, i.e. IDMAC did
    // drain the FIFO) -- confirmed on real hardware, where IDSTS stayed
    // 0x00000000 for the whole poll while RINTSTS correctly showed DTO.
    // RINTSTS.DTO (protocol-level "data transfer over") is the reliable
    // completion signal here instead.
    let mut timeout = 2_000_000u32;
    let ok = loop {
        let raw = unsafe { read(RINTSTS) };
        if raw & RINT_DATA_TRANSFER_OVER != 0 {
            unsafe { write(RINTSTS, RINT_ALL) };
            let failed = raw
                & (RINT_DATA_CRC_ERROR
                    | RINT_DATA_READ_TIMEOUT
                    | RINT_FIFO_ERROR
                    | RINT_START_BIT_ERROR);
            if failed != 0 {
                uart::log_hex(b"SDMMC: CMD17 data transfer error, RINTSTS=", raw);
                break false;
            }
            break true;
        }
        if timeout == 0 {
            uart::log_hex(b"SDMMC: CMD17 data-transfer-over timed out, RINTSTS=", raw);
            unsafe { write(RINTSTS, RINT_ALL) };
            break false;
        }
        timeout -= 1;
    };
    unsafe { write(IDSTS, IDSTS_ALL) }; // clear any stray DMA status bits regardless

    unsafe { modify(CTRL, CTRL_USE_INTERNAL_DMA, 0) };
    if !ok {
        return false;
    }

    cache_writeback_invalidate(buffer.as_ptr() as usize, BLOCK_BYTES);
    true
}

/// Reads consecutive blocks via CMD18 (`READ_MULTIPLE_BLOCK`) into `buffer`,
/// whose length must be a nonzero multiple of 512 bytes (and small enough to
/// fit `MAX_DESCRIPTORS` chained descriptors, i.e. at most 64 KiB).
pub fn read_blocks(card: &SdCard, lba: u32, buffer: &mut [u8]) -> bool {
    transfer_blocks(card, lba, buffer, false)
}

/// Writes consecutive blocks via CMD25 (`WRITE_MULTIPLE_BLOCK`) from
/// `buffer`, with the same length constraints as `read_blocks`. Callers are
/// responsible for not overwriting data they care about -- there is no
/// partition/filesystem awareness at this layer.
pub fn write_blocks(card: &SdCard, lba: u32, buffer: &mut [u8]) -> bool {
    transfer_blocks(card, lba, buffer, true)
}

fn transfer_blocks(card: &SdCard, lba: u32, buffer: &mut [u8], is_write: bool) -> bool {
    if buffer.is_empty() || buffer.len() % BLOCK_BYTES != 0 {
        uart::log(b"SDMMC: block transfer length must be a nonzero multiple of 512 bytes\r\n");
        return false;
    }
    let address = if card.high_capacity {
        lba
    } else {
        lba.wrapping_mul(BLOCK_BYTES as u32)
    };

    // For a write this flushes the source data out to RAM before DMA reads
    // it; for a read it drops any stale dirty lines that could otherwise
    // later clobber what DMA is about to write (same reasoning as
    // `read_block`).
    cache_writeback_invalidate(buffer.as_ptr() as usize, buffer.len());

    let mut descriptors = [Descriptor::zeroed(); MAX_DESCRIPTORS];
    let Some(descriptor_count) = build_descriptor_chain(&mut descriptors, buffer) else {
        uart::log(b"SDMMC: too many blocks for one DMA descriptor chain\r\n");
        return false;
    };
    let descriptors_address = descriptors.as_mut_ptr() as usize;
    cache_writeback_invalidate(descriptors_address, descriptor_count * size_of::<Descriptor>());

    unsafe {
        write(BLKSIZ, BLOCK_BYTES as u32);
        write(BYTCNT, buffer.len() as u32);
        // The card-read-threshold trick `read_block` needed to make IDMAC's
        // FIFO-to-RAM burst engine start; `CARDWRTHREN` (bit 2) is
        // documented as HS400-only, so it is left off for writes.
        write(
            CARDTHRCTL,
            if is_write {
                0
            } else {
                (BLOCK_BYTES as u32) << 16 | CARDTHRCTL_CARDRDTHREN
            },
        );
        modify(CTRL, CTRL_USE_INTERNAL_DMA, CTRL_USE_INTERNAL_DMA);
        modify(BMOD, BMOD_DE | BMOD_FB, BMOD_DE | BMOD_FB);
        write(DBADDR, descriptors_address as u32);
        write(PLDMND, 1);
    }

    let index = if is_write { 25 } else { 18 };
    let mut flags = CMD_RESPONSE_EXPECT
        | CMD_CHECK_RESPONSE_CRC
        | CMD_DATA_EXPECTED
        | CMD_WAIT_PRVDATA_COMPLETE
        | CMD_SEND_AUTO_STOP;
    if is_write {
        flags |= CMD_READ_WRITE;
    }
    if send_command(index, address, flags).is_err() {
        uart::log(b"SDMMC: CMD18/CMD25 (multi-block transfer) failed\r\n");
        return false;
    }

    // Same reasoning as `read_block`: RINTSTS.DTO is the reliable completion
    // signal, not IDSTS. Auto-stop's own completion (RINTSTS.ACD) is not
    // polled separately -- by the time DTO fires the data is already
    // transferred, which is what callers care about.
    let mut timeout = 4_000_000u32; // more blocks need more time than a single one
    let ok = loop {
        let raw = unsafe { read(RINTSTS) };
        if raw & RINT_DATA_TRANSFER_OVER != 0 {
            unsafe { write(RINTSTS, RINT_ALL) };
            let failed = raw
                & (RINT_DATA_CRC_ERROR
                    | RINT_DATA_READ_TIMEOUT
                    | RINT_FIFO_ERROR
                    | RINT_START_BIT_ERROR);
            if failed != 0 {
                uart::log_hex(b"SDMMC: multi-block transfer error, RINTSTS=", raw);
                break false;
            }
            break true;
        }
        if timeout == 0 {
            uart::log_hex(b"SDMMC: multi-block transfer timed out, RINTSTS=", raw);
            unsafe { write(RINTSTS, RINT_ALL) };
            break false;
        }
        timeout -= 1;
    };

    unsafe {
        write(IDSTS, IDSTS_ALL);
        modify(CTRL, CTRL_USE_INTERNAL_DMA, 0);
    }
    if !ok {
        return false;
    }

    // RINTSTS.DTO only means the host-to-card (or card-to-host) data phase
    // finished; a write's actual flash programming keeps the card busy
    // (DAT0 held low) afterward. Sending the next command before that clears
    // got no response at all (RTO) on real hardware -- confirmed with
    // back-to-back write/read/write calls in `sdwritetest`.
    wait_data_not_busy();

    if !is_write {
        cache_writeback_invalidate(buffer.as_ptr() as usize, buffer.len());
    }
    true
}

/// Fills `descriptors` with a chain covering all of `buffer`, each holding
/// up to `DESC_MAX_BUFFER_BYTES`. Returns the number of descriptors used, or
/// `None` if `buffer` needs more than `descriptors` has room for.
fn build_descriptor_chain(
    descriptors: &mut [Descriptor; MAX_DESCRIPTORS],
    buffer: &mut [u8],
) -> Option<usize> {
    let total = buffer.len();
    let count = total.div_ceil(DESC_MAX_BUFFER_BYTES);
    if count == 0 || count > MAX_DESCRIPTORS {
        return None;
    }

    let descriptors_base = descriptors.as_mut_ptr();
    let buffer_base = buffer.as_mut_ptr();
    let mut offset = 0usize;
    for i in 0..count {
        let chunk = (total - offset).min(DESC_MAX_BUFFER_BYTES);
        let is_last = i + 1 == count;
        descriptors[i] = Descriptor {
            control: DESC_OWNED_BY_IDMAC
                | DESC_CHAINED
                | if i == 0 { DESC_FIRST } else { 0 }
                | if is_last { DESC_LAST } else { 0 },
            buffer1_size: chunk as u32,
            buffer1_ptr: unsafe { buffer_base.add(offset) } as u32,
            next_or_buffer2_ptr: if is_last {
                0
            } else {
                unsafe { descriptors_base.add(i + 1) as u32 }
            },
            reserved: [0; 12],
        };
        offset += chunk;
    }
    Some(count)
}

/// Logs a 512-byte block as 32 rows of 16 space-separated hex bytes.
pub fn dump_block(buffer: &[u8; BLOCK_BYTES]) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for row in buffer.chunks(16) {
        let mut line = [0u8; 16 * 3 + 2];
        let mut pos = 0;
        for &byte in row {
            line[pos] = HEX[(byte >> 4) as usize];
            line[pos + 1] = HEX[(byte & 0xF) as usize];
            line[pos + 2] = b' ';
            pos += 3;
        }
        line[pos] = b'\r';
        line[pos + 1] = b'\n';
        uart::log(&line[..pos + 2]);
    }
}

/// SD Physical Layer Spec CSD version 2.0 (SDHC/SDXC): `C_SIZE` is bits
/// [69:48] of the 128-bit CSD, and capacity is `(C_SIZE + 1) * 512 KiB`.
/// Returns `None` for CSD version 1.0 (byte-addressed standard-capacity)
/// cards, which use a different, more involved formula this stage does not
/// need yet.
fn decode_csd_v2_capacity(csd: [u32; 4]) -> Option<u64> {
    if csd[3] >> 30 != 1 {
        return None;
    }
    let c_size = ((csd[2] & 0x3F) << 16) | (csd[1] >> 16);
    Some((c_size as u64 + 1) * 512 * 1024)
}

fn dump_diagnostics() {
    unsafe {
        uart::log_hex(b"SDMMC: CTRL=", read(CTRL));
        uart::log_hex(b"SDMMC: CLKDIV=", read(CLKDIV));
        uart::log_hex(b"SDMMC: CLKSRC=", read(CLKSRC));
        uart::log_hex(b"SDMMC: CLKENA=", read(CLKENA));
        uart::log_hex(b"SDMMC: STATUS=", read(STATUS));
        uart::log_hex(b"SDMMC: RINTSTS=", read(RINTSTS));
        uart::log_hex(b"SDMMC: PERI_CLK_CTRL01=", read(PERI_CLK_CTRL01));
        uart::log_hex(b"SDMMC: PERI_CLK_CTRL02=", read(PERI_CLK_CTRL02));
    }
}

fn enable_bus_clock() {
    unsafe { modify(SOC_CLK_CTRL1, 1 << 14, 1 << 14) };
}

fn reset_peripheral() {
    unsafe {
        modify(HP_SDMMC_EMAC_RST_CTRL, 1 << 28, 1 << 28);
        modify(HP_SDMMC_EMAC_RST_CTRL, 1 << 28, 0);
    }
}

fn configure_pins() {
    for pin in [PIN_CLK, PIN_CMD, PIN_D0, PIN_D1, PIN_D2, PIN_D3] {
        unsafe { modify(iomux_register(pin), 0x7 << 12, SDMMC_IOMUX_FUNC << 12) };
    }
    // CMD/D0..D3 are bidirectional; the pad's input buffer (`fun_ie`) resets
    // to disabled and, unlike output enable, is not implicitly switched on
    // by selecting a non-GPIO `mcu_sel` function, so it needs setting
    // explicitly or the controller's receiver only ever sees a stuck 0.
    // Also match the ESP-IDF driver's `gpio_pullup_en` for these pins
    // (CLK is host-driven output only, so it needs neither).
    for pin in [PIN_CMD, PIN_D0, PIN_D1, PIN_D2, PIN_D3] {
        unsafe { modify(iomux_register(pin), (1 << 8) | (1 << 9), (1 << 8) | (1 << 9)) };
    }
}

fn iomux_register(pin: u32) -> usize {
    IO_MUX + 0x04 + pin as usize * 4
}

/// Configures the SDIO low-speed clock generator (`HP_SYS_CLKRST`) that
/// feeds `sdhost_cclk_in`: source PLL160M, divided by `div`.
fn set_low_speed_clock_source(div: u32) {
    unsafe {
        if div > 1 {
            modify(
                PERI_CLK_CTRL02,
                (0xF << 9) | (0xF << 13) | (0xF << 17),
                ((div - 1) << 9) | ((div / 2 - 1) << 13) | ((div - 1) << 17),
            );
            modify(PERI_CLK_CTRL02, 1 << 8, 1 << 8);
            modify(PERI_CLK_CTRL02, 1 << 8, 0);
        } else {
            modify(PERI_CLK_CTRL01, 1 << 22, 1 << 22); // high-speed bypass mode
            modify(PERI_CLK_CTRL02, (0xF << 9) | (0xF << 13) | (0xF << 17), 0);
        }
        // Source 0 = PLL160M (matches `sdmmc_ll_select_clk_source`'s SDMMC_CLK_SRC_PLL160M).
        modify(PERI_CLK_CTRL01, 1 << 23, 0);
        modify(PERI_CLK_CTRL01, 1 << 24, 1 << 24);
        modify(
            PERI_CLK_CTRL02,
            (1 << 27) | (1 << 28) | (1 << 29) | (0x3 << 21) | (0x3 << 23) | (0x3 << 25),
            (1 << 27) | (1 << 28) | (1 << 29) | (1 << 23),
        );
        modify(PERI_CLK_CTRL02, 1 << 8, 1 << 8);
        modify(PERI_CLK_CTRL02, 1 << 8, 0);
    }
    delay_us(10);
}

fn reset_controller() -> bool {
    unsafe { write(CTRL, (1 << 0) | (1 << 1) | (1 << 2)) };
    let mut timeout = 100_000u32;
    while unsafe { read(CTRL) } & 0x7 != 0 {
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
    }
    true
}

/// One-time IDMAC bring-up (matches `sdmmc_ll_init_dma`): the controller-wide
/// DMA enable bit and a burst-mode-controller software reset. Per-transfer
/// setup (descriptor, `use_internal_dma`, `BMOD_DE`/`BMOD_FB`) happens in
/// `read_block`.
fn init_dma() {
    unsafe {
        // ESP-IDF's `sdmmc_host_init` always sets this (`sdmmc_ll_enable_global_interrupt`)
        // as part of bringing up interrupt-driven DMA. This project never routes
        // SDIO_HOST_INTR to a CPU interrupt line (see `interrupts.rs`, which only
        // wires up the LCD's DW-GDMA source), so the peripheral's own interrupt
        // output has nowhere to go and setting this cannot cause a spurious trap;
        // it is included in case IDMAC's raw status latching depends on it.
        modify(CTRL, CTRL_INT_ENABLE, CTRL_INT_ENABLE);
        modify(CTRL, CTRL_DMA_ENABLE, CTRL_DMA_ENABLE);
        write(BMOD, BMOD_SWR);
    }
    delay_us(10); // BMOD_SWR self-clears after one clock; this is generous.
}

/// Writes back dirty cache lines over `address..address+length` and
/// invalidates them, so a DMA engine reading `address` sees what the CPU
/// last wrote there, and a later CPU read of `address` sees what DMA last
/// wrote there. Mirrors `psram.rs`'s `writeback_range`, but for internal
/// SRAM rather than PSRAM.
fn cache_writeback_invalidate(address: usize, length: usize) {
    let writeback_invalidate: unsafe extern "C" fn(u32, u32, u32) -> i32 =
        unsafe { core::mem::transmute(ROM_CACHE_WRITEBACK_INVALIDATE_ADDR) };
    unsafe {
        writeback_invalidate(CACHE_MAP_L1_DCACHE, address as u32, length as u32);
        writeback_invalidate(CACHE_MAP_L2_CACHE, address as u32, length as u32);
    }
}

/// Programs slot 0's card clock divider (`SDHOST_CLKDIV_REG`'s divisor is
/// `2*card_div`) and propagates it into the CIU via the clock-update
/// pseudo-command, matching `sdmmc_host_set_card_clk`'s disable/update,
/// configure/update, enable/update sequence.
fn set_card_clock(host_div: u32, card_div: u32) -> bool {
    unsafe { modify(CLKENA, 0x3 | (0x3 << 16), 0) };
    if !update_clock_registers() {
        return false;
    }

    set_low_speed_clock_source(host_div);
    unsafe {
        modify(CLKSRC, 0x3, 0); // card0 uses clock divider 0
        modify(CLKDIV, 0xFF, card_div);
    }
    if !update_clock_registers() {
        return false;
    }

    unsafe { modify(CLKENA, 0x3 | (0x3 << 16), 0x1 | (0x1 << 16)) };
    update_clock_registers()
}

/// The register comment on `SDHOST_UPDATE_CLOCK_REGISTERS_ONLY` is explicit
/// that this pseudo-command never raises a Command Done interrupt, since no
/// command actually reaches the card; ESP-IDF's own
/// `sdmmc_host_clock_update_command` correspondingly only waits for the CIU
/// to accept it (`sdmmc_host_start_command`), never for completion. Using
/// `send_command`'s response wait here would time out on every call.
fn update_clock_registers() -> bool {
    let flags = CMD_UPDATE_CLOCK_REGISTERS_ONLY | CMD_WAIT_PRVDATA_COMPLETE;
    start_command(0, 0, flags)
}

/// Writes the command into the CIU and waits for it to be accepted (i.e.
/// `START_CMD` self-clears). This alone is enough for the clock-update
/// pseudo-command; real SD commands additionally need `send_command`'s wait
/// for the completion event and response.
fn start_command(index: u32, arg: u32, flags: u32) -> bool {
    if !wait_command_taken(10_000) {
        return false;
    }
    unsafe {
        write(CMDARG, arg);
        write(CMD, (index & 0x3F) | flags | CMD_USE_HOLD_REG | CMD_START);
    }
    wait_command_taken(10_000)
}

/// Sends one command, waits for completion, and returns the response
/// registers (all four for a long response, only `[0]` meaningful
/// otherwise). `check_crc` is folded into `flags` by the caller; long
/// responses (CID/CSD, R2) still carry a valid CRC7 and should set it,
/// while ACMD41's R3 response has no CRC and must not.
fn send_command(index: u32, arg: u32, flags: u32) -> Result<[u32; 4], ()> {
    if !start_command(index, arg, flags) {
        return Err(());
    }

    let mut timeout = 2_000_000u32;
    loop {
        let raw = unsafe { read(RINTSTS) };
        if raw & RINT_COMMAND_DONE != 0 {
            unsafe { write(RINTSTS, RINT_ALL) };
            let failed = raw
                & (RINT_RESPONSE_ERROR
                    | RINT_RESPONSE_CRC_ERROR
                    | RINT_RESPONSE_TIMEOUT
                    | RINT_HARDWARE_LOCKED_ERROR);
            if failed != 0 {
                uart::log_hex(b"SDMMC: command failed, RINTSTS=", raw);
                uart::log_hex(b"SDMMC: command failed, RESP0=", unsafe { read(RESP0) });
                let status = unsafe { read(STATUS) };
                uart::log_hex(b"SDMMC: command failed, STATUS=", status);
                uart::log_hex(
                    b"SDMMC: command failed, response index=",
                    (status >> 11) & 0x3F,
                );
                return Err(());
            }
            break;
        }
        if timeout == 0 {
            uart::log_hex(b"SDMMC: command hard timeout, RINTSTS=", raw);
            unsafe { write(RINTSTS, RINT_ALL) };
            return Err(());
        }
        timeout -= 1;
    }

    let mut response = [0u32; 4];
    for (index, slot) in response.iter_mut().enumerate() {
        *slot = unsafe { read(RESP0 + index * 4) };
    }
    Ok(response)
}

fn wait_command_taken(iterations: u32) -> bool {
    let mut timeout = iterations;
    while unsafe { read(CMD) } & CMD_START != 0 {
        if timeout == 0 {
            return false;
        }
        timeout -= 1;
    }
    true
}

/// Polls `STATUS.DATA_BUSY` (DAT0 held low) clear. Used after CMD7 (brief)
/// and after write transfers, where the card can legitimately stay busy
/// programming flash for a long time -- SD spec allows up to 250ms per
/// block, so the budget here is generous rather than tuned to any one call
/// site.
fn wait_data_not_busy() {
    let mut timeout = 200_000_000u32; // ~550ms at 360 MHz
    while unsafe { read(STATUS) } & (1 << 9) != 0 {
        if timeout == 0 {
            uart::log(b"SDMMC: card stayed busy\r\n");
            return;
        }
        timeout -= 1;
    }
}

fn delay_us(microseconds: u32) {
    const CPU_CYCLES_PER_US: u32 = 360; // matches `startup::raise_cpu_clock`'s 360 MHz
    let start = cycle_count();
    while cycle_count().wrapping_sub(start) < microseconds.saturating_mul(CPU_CYCLES_PER_US) {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn cycle_count() -> u32 {
    let value: u32;
    unsafe {
        core::arch::asm!("rdcycle {value}", value = out(reg) value, options(nomem, nostack));
    }
    value
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn read(address: usize) -> u32 {
    unsafe { (address as *const u32).read_volatile() }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn write(address: usize, value: u32) {
    unsafe { (address as *mut u32).write_volatile(value) }
}

/// # Safety
/// `address` must be a valid, mapped, 4-byte-aligned MMIO register.
unsafe fn modify(address: usize, mask: u32, value: u32) {
    unsafe {
        write(address, (read(address) & !mask) | (value & mask));
    }
}
