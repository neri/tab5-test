//! Read-only decoding of the ESP32-P4 Physical Memory Attribute table.
//!
//! PMA CSRs use a four-byte address granule.  The configuration CSRs are
//! one-per-entry on this core, rather than the packed layout used by PMP.

/// Number of PMA entries implemented by the ESP32-P4 CPU.
pub(crate) const ENTRY_COUNT: usize = 16;

const PMA_ENABLE: u32 = 1 << 0;
const PMA_EXECUTE: u32 = 1 << 2;
const PMA_WRITE: u32 = 1 << 3;
const PMA_READ: u32 = 1 << 4;
const PMA_READ_MISS_NO_ALLOC: u32 = 1 << 24;
const PMA_WRITE_MISS_NO_ALLOC: u32 = 1 << 25;
const PMA_WRITE_THROUGH: u32 = 1 << 26;
const PMA_NON_CACHEABLE: u32 = 1 << 27;
const PMA_LOCK: u32 = 1 << 29;
const PMA_MODE_MASK: u32 = 0xC000_0000;
const PMA_TOR: u32 = 0x4000_0000;
const PMA_NA4: u32 = 0x8000_0000;
const PMA_NAPOT: u32 = 0xC000_0000;

/// A decoded PMA entry and, where applicable, the range it matches.
#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) index: usize,
    pub(crate) config: u32,
    pub(crate) address: u32,
    pub(crate) range: Option<Range>,
}

/// A half-open physical-address range.  `end` may be above `u32::MAX` for a
/// PMA entry that reaches the end of the 32-bit address space.
#[derive(Clone, Copy)]
pub(crate) struct Range {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl Entry {
    /// The entry's addressing scheme.
    pub(crate) fn mode_name(self) -> &'static str {
        match self.config & PMA_MODE_MASK {
            PMA_TOR => "TOR",
            PMA_NA4 => "NA4",
            PMA_NAPOT => "NAPOT",
            _ => "OFF",
        }
    }

    pub(crate) fn enabled(self) -> bool {
        self.config & PMA_ENABLE != 0
    }

    pub(crate) fn locked(self) -> bool {
        self.config & PMA_LOCK != 0
    }

    pub(crate) fn readable(self) -> bool {
        self.config & PMA_READ != 0
    }

    pub(crate) fn writable(self) -> bool {
        self.config & PMA_WRITE != 0
    }

    pub(crate) fn executable(self) -> bool {
        self.config & PMA_EXECUTE != 0
    }

    pub(crate) fn non_cacheable(self) -> bool {
        self.config & PMA_NON_CACHEABLE != 0
    }

    pub(crate) fn write_through(self) -> bool {
        self.config & PMA_WRITE_THROUGH != 0
    }

    pub(crate) fn write_miss_no_alloc(self) -> bool {
        self.config & PMA_WRITE_MISS_NO_ALLOC != 0
    }

    pub(crate) fn read_miss_no_alloc(self) -> bool {
        self.config & PMA_READ_MISS_NO_ALLOC != 0
    }

    /// The address represented by `pmaaddrN` when it is not itself a range.
    /// For an OFF entry this is the lower bound used by a following TOR entry.
    pub(crate) fn address_bytes(self) -> u64 {
        (self.address as u64) << 2
    }
}

macro_rules! read_pma_csrs {
    ($config_csr:literal, $address_csr:literal) => {{
        let config: u32;
        let address: u32;
        unsafe {
            core::arch::asm!(
                "csrr {config}, {config_csr}",
                "csrr {address}, {address_csr}",
                config = out(reg) config,
                address = out(reg) address,
                config_csr = const $config_csr,
                address_csr = const $address_csr,
                options(nomem, nostack),
            );
        }
        (config, address)
    }};
}

/// Reads and decodes all PMA entries.  This function only reads CSRs; it
/// never attempts to alter the bootloader-owned, locked PMA table.
pub(crate) fn entries() -> [Entry; ENTRY_COUNT] {
    let raw = [
        read_pma_csrs!(0xBC0, 0xBD0),
        read_pma_csrs!(0xBC1, 0xBD1),
        read_pma_csrs!(0xBC2, 0xBD2),
        read_pma_csrs!(0xBC3, 0xBD3),
        read_pma_csrs!(0xBC4, 0xBD4),
        read_pma_csrs!(0xBC5, 0xBD5),
        read_pma_csrs!(0xBC6, 0xBD6),
        read_pma_csrs!(0xBC7, 0xBD7),
        read_pma_csrs!(0xBC8, 0xBD8),
        read_pma_csrs!(0xBC9, 0xBD9),
        read_pma_csrs!(0xBCA, 0xBDA),
        read_pma_csrs!(0xBCB, 0xBDB),
        read_pma_csrs!(0xBCC, 0xBDC),
        read_pma_csrs!(0xBCD, 0xBDD),
        read_pma_csrs!(0xBCE, 0xBDE),
        read_pma_csrs!(0xBCF, 0xBDF),
    ];

    let mut decoded = [Entry {
        index: 0,
        config: 0,
        address: 0,
        range: None,
    }; ENTRY_COUNT];
    for index in 0..ENTRY_COUNT {
        let previous_address = if index == 0 { 0 } else { raw[index - 1].1 };
        decoded[index] = decode(index, raw[index].0, raw[index].1, previous_address);
    }
    decoded
}

fn decode(index: usize, config: u32, address: u32, previous_address: u32) -> Entry {
    let range = match config & PMA_MODE_MASK {
        PMA_TOR => Some(Range {
            start: (previous_address as u64) << 2,
            end: (address as u64) << 2,
        }),
        PMA_NA4 => {
            let start = (address as u64) << 2;
            Some(Range {
                start,
                end: start + 4,
            })
        }
        PMA_NAPOT => {
            // The contiguous low one bits encode log2(size) - 3.  Use u64
            // throughout so even the all-address-space encoding stays exact.
            let ones = address.trailing_ones();
            let low_ones = (1u64 << ones) - 1;
            let start = ((address as u64) & !low_ones) << 2;
            Some(Range {
                start,
                end: start + (1u64 << (ones + 3)),
            })
        }
        _ => None,
    };
    Entry {
        index,
        config,
        address,
        range,
    }
}
