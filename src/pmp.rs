//! Read-only decoding of the RISC-V Physical Memory Protection table.
//!
//! PMP is the standard privileged-architecture counterpart to the
//! ESP32-P4-specific PMA table decoded by `pma`: it grants R/W/X permission
//! for an address range, where PMA describes how that range behaves (cache
//! policy and the like).  The two tables are read the same way but are not
//! laid out the same way -- PMP packs four entries' configuration bytes into
//! one `pmpcfgN` CSR, whereas this core gives PMA one CSR per entry.
//!
//! Entries are priority-ordered: the lowest-numbered *matching* entry decides
//! access, so a later entry that overlaps an earlier one contributes nothing.
//! Machine mode ignores entries whose lock bit is clear, and an access that
//! matches no entry at all is permitted in machine mode -- which is the mode
//! this firmware runs in throughout.
//!
//! Like `pma`, this module only reads CSRs.  The bootloader
//! (`esp_cpu_configure_region_protection`) installs and locks this table
//! before the firmware starts.

/// Number of PMP entries implemented by the ESP32-P4 CPU.
pub(crate) const ENTRY_COUNT: usize = 16;

/// Address granularity in bytes, as reported by ESP-IDF's
/// `SOC_CPU_PMP_REGION_GRANULARITY`.  It is larger than four bytes, so NA4 is
/// not usable on this core and the low bits of `pmpaddrN` are read-only.
pub(crate) const GRANULARITY: u32 = 128;

const PMP_READ: u8 = 1 << 0;
const PMP_WRITE: u8 = 1 << 1;
const PMP_EXECUTE: u8 = 1 << 2;
const PMP_LOCK: u8 = 1 << 7;
const PMP_MODE_MASK: u8 = 0x18;
const PMP_TOR: u8 = 0x08;
const PMP_NA4: u8 = 0x10;
const PMP_NAPOT: u8 = 0x18;

/// A decoded PMP entry and, where applicable, the range it matches.
#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) index: usize,
    pub(crate) config: u8,
    pub(crate) address: u32,
    pub(crate) range: Option<Range>,
}

/// A half-open physical-address range.  `end` may be above `u32::MAX` for an
/// entry that reaches the end of the 32-bit address space.
#[derive(Clone, Copy)]
pub(crate) struct Range {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

impl Entry {
    /// The entry's addressing scheme.
    pub(crate) fn mode_name(self) -> &'static str {
        match self.config & PMP_MODE_MASK {
            PMP_TOR => "TOR",
            PMP_NA4 => "NA4",
            PMP_NAPOT => "NAPOT",
            _ => "OFF",
        }
    }

    pub(crate) fn locked(self) -> bool {
        self.config & PMP_LOCK != 0
    }

    pub(crate) fn readable(self) -> bool {
        self.config & PMP_READ != 0
    }

    pub(crate) fn writable(self) -> bool {
        self.config & PMP_WRITE != 0
    }

    pub(crate) fn executable(self) -> bool {
        self.config & PMP_EXECUTE != 0
    }

    /// The address represented by `pmpaddrN` when it is not itself a range.
    /// For an OFF entry this is the lower bound used by a following TOR entry.
    pub(crate) fn address_bytes(self) -> u64 {
        (self.address as u64) << 2
    }
}

impl Range {
    /// A TOR entry whose bound is at or below the previous entry's bound
    /// matches nothing at all, however permissive its configuration byte.
    pub(crate) fn is_empty(self) -> bool {
        self.end <= self.start
    }
}

macro_rules! read_csr {
    ($csr:literal) => {{
        let value: u32;
        unsafe {
            core::arch::asm!(
                "csrr {value}, {csr}",
                value = out(reg) value,
                csr = const $csr,
                options(nomem, nostack),
            );
        }
        value
    }};
}

/// Reads and decodes all PMP entries.
pub(crate) fn entries() -> [Entry; ENTRY_COUNT] {
    // Four entries per `pmpcfgN` CSR, least-significant byte first.
    let config_words = [
        read_csr!(0x3A0),
        read_csr!(0x3A1),
        read_csr!(0x3A2),
        read_csr!(0x3A3),
    ];
    let addresses = [
        read_csr!(0x3B0),
        read_csr!(0x3B1),
        read_csr!(0x3B2),
        read_csr!(0x3B3),
        read_csr!(0x3B4),
        read_csr!(0x3B5),
        read_csr!(0x3B6),
        read_csr!(0x3B7),
        read_csr!(0x3B8),
        read_csr!(0x3B9),
        read_csr!(0x3BA),
        read_csr!(0x3BB),
        read_csr!(0x3BC),
        read_csr!(0x3BD),
        read_csr!(0x3BE),
        read_csr!(0x3BF),
    ];

    let mut decoded = [Entry {
        index: 0,
        config: 0,
        address: 0,
        range: None,
    }; ENTRY_COUNT];
    for index in 0..ENTRY_COUNT {
        let config = (config_words[index / 4] >> ((index % 4) * 8)) as u8;
        let previous_address = if index == 0 { 0 } else { addresses[index - 1] };
        decoded[index] = decode(index, config, addresses[index], previous_address);
    }
    decoded
}

fn decode(index: usize, config: u8, address: u32, previous_address: u32) -> Entry {
    let range = match config & PMP_MODE_MASK {
        PMP_TOR => Some(Range {
            start: (previous_address as u64) << 2,
            end: (address as u64) << 2,
        }),
        PMP_NA4 => {
            let start = (address as u64) << 2;
            Some(Range {
                start,
                end: start + 4,
            })
        }
        PMP_NAPOT => {
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
