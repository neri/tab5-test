#![no_std]

//! Pure validation for USB Mass Storage Bulk-Only Transport status wrappers.
//!
//! Hardware transfer and Reset Recovery remain in the firmware. This crate
//! owns the byte-level boundary between a received CSW and a command result,
//! so malformed wrappers can be exhaustively tested on the host.

pub const CSW_LEN: usize = 13;
pub const CSW_SIGNATURE: u32 = u32::from_le_bytes(*b"USBS");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataDirection {
    None,
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransferSummary {
    pub direction: DataDirection,
    pub expected: usize,
    pub host_actual: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutLengthError {
    pub expected: usize,
    pub actual: usize,
}

/// Enforces exact length at the CBW and data-OUT phase boundaries.
pub fn validate_exact_out(expected: usize, actual: usize) -> Result<(), OutLengthError> {
    if actual == expected {
        Ok(())
    } else {
        Err(OutLengthError { expected, actual })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandStatus {
    Passed,
    Failed,
}

impl CommandStatus {
    pub const fn as_u8(self) -> u8 {
        match self {
            Self::Passed => 0,
            Self::Failed => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedCsw {
    pub tag: u32,
    pub residue: u32,
    pub status: CommandStatus,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Length {
        received: usize,
    },
    BadSignature {
        received: u32,
    },
    TagMismatch {
        expected: u32,
        received: u32,
    },
    PhaseError,
    InvalidStatus {
        received: u8,
    },
    ResidueExceedsExpected {
        residue: u32,
        expected: usize,
    },
    HostActualExceedsExpected {
        actual: usize,
        expected: usize,
    },
    InLengthMismatch {
        actual: usize,
        expected_from_residue: usize,
    },
    OutLengthMismatch {
        actual: usize,
        expected: usize,
    },
}

/// Returns true only for a complete, structurally plausible CSW frame.
///
/// Tag and residue are intentionally not compared here. The caller uses this
/// to recognize both the current command's early CSW and a stale prior CSW in
/// the data-IN phase; full validation then reports which one it was.
pub fn looks_like_csw(bytes: &[u8]) -> bool {
    bytes.len() == CSW_LEN && word(bytes, 0) == CSW_SIGNATURE && matches!(bytes[12], 0..=2)
}

/// Parses and validates one CSW against what the host actually transferred.
///
/// For IN, residue describes bytes the device did not return, so
/// `host_actual == expected - residue` must hold. For OUT, the host must have
/// put the complete data phase on the bus, while nonzero residue can still
/// report that the device did not process all of it; command-specific code
/// decides whether such a result is acceptable.
pub fn validate_csw(
    bytes: &[u8],
    expected_tag: u32,
    transfer: TransferSummary,
) -> Result<ValidatedCsw, ValidationError> {
    if bytes.len() != CSW_LEN {
        return Err(ValidationError::Length {
            received: bytes.len(),
        });
    }
    let signature = word(bytes, 0);
    if signature != CSW_SIGNATURE {
        return Err(ValidationError::BadSignature {
            received: signature,
        });
    }
    let tag = word(bytes, 4);
    if tag != expected_tag {
        return Err(ValidationError::TagMismatch {
            expected: expected_tag,
            received: tag,
        });
    }
    let residue = word(bytes, 8);
    let status = match bytes[12] {
        0 => CommandStatus::Passed,
        1 => CommandStatus::Failed,
        2 => return Err(ValidationError::PhaseError),
        received => return Err(ValidationError::InvalidStatus { received }),
    };
    let residue_usize = residue as usize;
    if residue_usize > transfer.expected {
        return Err(ValidationError::ResidueExceedsExpected {
            residue,
            expected: transfer.expected,
        });
    }
    if transfer.host_actual > transfer.expected {
        return Err(ValidationError::HostActualExceedsExpected {
            actual: transfer.host_actual,
            expected: transfer.expected,
        });
    }
    match transfer.direction {
        DataDirection::None | DataDirection::In => {
            let expected_from_residue = transfer.expected - residue_usize;
            if transfer.host_actual != expected_from_residue {
                return Err(ValidationError::InLengthMismatch {
                    actual: transfer.host_actual,
                    expected_from_residue,
                });
            }
        }
        DataDirection::Out => {
            if transfer.host_actual != transfer.expected {
                return Err(ValidationError::OutLengthMismatch {
                    actual: transfer.host_actual,
                    expected: transfer.expected,
                });
            }
        }
    }
    Ok(ValidatedCsw {
        tag,
        residue,
        status,
    })
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csw(tag: u32, residue: u32, status: u8) -> [u8; CSW_LEN] {
        let mut bytes = [0u8; CSW_LEN];
        bytes[0..4].copy_from_slice(&CSW_SIGNATURE.to_le_bytes());
        bytes[4..8].copy_from_slice(&tag.to_le_bytes());
        bytes[8..12].copy_from_slice(&residue.to_le_bytes());
        bytes[12] = status;
        bytes
    }

    fn summary(direction: DataDirection, expected: usize, host_actual: usize) -> TransferSummary {
        TransferSummary {
            direction,
            expected,
            host_actual,
        }
    }

    #[test]
    fn accepts_normal_in_out_and_no_data_commands() {
        let cases = [
            (DataDirection::In, 512, 512, 0),
            (DataDirection::In, 36, 13, 23),
            (DataDirection::Out, 512, 512, 0),
            (DataDirection::Out, 512, 512, 128),
            (DataDirection::None, 0, 0, 0),
        ];
        for (direction, expected, actual, residue) in cases {
            let result =
                validate_csw(&csw(7, residue, 0), 7, summary(direction, expected, actual)).unwrap();
            assert_eq!(result.residue, residue);
            assert_eq!(result.status, CommandStatus::Passed);
        }
    }

    #[test]
    fn rejects_short_cbw_and_data_out() {
        let cases = [
            (31, 31, true),
            (31, 30, false),
            (512, 512, true),
            (512, 511, false),
        ];
        for (expected, actual, accepted) in cases {
            assert_eq!(validate_exact_out(expected, actual).is_ok(), accepted);
        }
    }

    #[test]
    fn rejects_every_non_exact_csw_length() {
        let bytes = csw(1, 0, 0);
        for length in [0, 12] {
            assert_eq!(
                validate_csw(&bytes[..length], 1, summary(DataDirection::None, 0, 0)),
                Err(ValidationError::Length { received: length })
            );
        }
        let mut long = [0u8; 14];
        long[..CSW_LEN].copy_from_slice(&bytes);
        assert_eq!(
            validate_csw(&long, 1, summary(DataDirection::None, 0, 0)),
            Err(ValidationError::Length { received: 14 })
        );
    }

    #[test]
    fn rejects_signature_tag_phase_and_undefined_status() {
        let transfer = summary(DataDirection::None, 0, 0);
        let mut bad_signature = csw(1, 0, 0);
        bad_signature[0] ^= 1;
        assert!(matches!(
            validate_csw(&bad_signature, 1, transfer),
            Err(ValidationError::BadSignature { .. })
        ));
        assert!(matches!(
            validate_csw(&csw(0, 0, 0), 1, transfer),
            Err(ValidationError::TagMismatch { .. })
        ));
        assert_eq!(
            validate_csw(&csw(1, 0, 2), 1, transfer),
            Err(ValidationError::PhaseError)
        );
        assert_eq!(
            validate_csw(&csw(1, 0, 3), 1, transfer),
            Err(ValidationError::InvalidStatus { received: 3 })
        );
        assert_eq!(
            validate_csw(&csw(1, 0, 1), 1, transfer).unwrap().status,
            CommandStatus::Failed
        );
    }

    #[test]
    fn rejects_residue_and_host_length_contradictions() {
        let cases = [
            (
                csw(1, 513, 0),
                summary(DataDirection::In, 512, 0),
                ValidationError::ResidueExceedsExpected {
                    residue: 513,
                    expected: 512,
                },
            ),
            (
                csw(1, 0, 0),
                summary(DataDirection::In, 8, 9),
                ValidationError::HostActualExceedsExpected {
                    actual: 9,
                    expected: 8,
                },
            ),
            (
                csw(1, 4, 0),
                summary(DataDirection::In, 8, 8),
                ValidationError::InLengthMismatch {
                    actual: 8,
                    expected_from_residue: 4,
                },
            ),
            (
                csw(1, 0, 0),
                summary(DataDirection::Out, 512, 511),
                ValidationError::OutLengthMismatch {
                    actual: 511,
                    expected: 512,
                },
            ),
        ];
        for (bytes, transfer, expected_error) in cases {
            assert_eq!(validate_csw(&bytes, 1, transfer), Err(expected_error));
        }
    }

    #[test]
    fn recognizes_current_and_stale_early_csw_but_not_payload() {
        assert!(looks_like_csw(&csw(9, 36, 1)));
        assert!(looks_like_csw(&csw(8, 0, 0)));
        let mut payload = csw(9, 36, 1);
        payload[0..4].copy_from_slice(b"DATA");
        assert!(!looks_like_csw(&payload));
        assert!(!looks_like_csw(&csw(9, 36, 3)));
        assert!(!looks_like_csw(&csw(9, 36, 1)[..12]));
    }

    #[test]
    fn early_current_csw_is_valid_only_with_matching_full_residue() {
        let transfer = summary(DataDirection::In, 36, 0);
        assert!(validate_csw(&csw(4, 36, 1), 4, transfer).is_ok());
        assert!(matches!(
            validate_csw(&csw(3, 36, 1), 4, transfer),
            Err(ValidationError::TagMismatch { .. })
        ));
        assert!(matches!(
            validate_csw(&csw(4, 0, 1), 4, transfer),
            Err(ValidationError::InLengthMismatch { .. })
        ));
    }
}
