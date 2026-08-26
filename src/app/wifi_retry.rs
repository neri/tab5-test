//! Pure retry classification for station association and reconnection.
//!
//! This module knows no clocks, RPC handles or credentials.  Given a reason
//! and the number of consecutive failed attempts, it only says whether to
//! stop or how long the manager should wait before the next attempt.

const SHORT_REASON_4_MS: u32 = 500;
const FIRST_GENERAL_MS: u32 = 1_000;
const MAX_GENERAL_MS: u32 = 30_000;
const STABLE_CONNECTION_MS: u64 = 10 * 60 * 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Stop,
    RetryAfter(u32),
}

pub fn after_disconnect(reason: u32, failed_attempts: u32) -> Decision {
    match reason {
        // Wrong credentials or an incompatible security mode will not heal
        // by sending the same secret again.
        15 | 202 | 204 | 210 | 211 => Decision::Stop,
        // The reboot/AP stale-entry case gets three quick chances before it
        // joins the ordinary exponential schedule.
        4 if failed_attempts <= 3 => Decision::RetryAfter(SHORT_REASON_4_MS),
        4 => Decision::RetryAfter(general_delay(failed_attempts - 3)),
        // Radio/AP availability and generic association failures.
        200 | 201 | 203 | 205 | 212 => Decision::RetryAfter(general_delay(failed_attempts)),
        // Unknown reasons stay recoverable, but never become a tight loop.
        _ => Decision::RetryAfter(general_delay(failed_attempts)),
    }
}

pub fn after_timeout(failed_attempts: u32) -> Decision {
    Decision::RetryAfter(general_delay(failed_attempts))
}

pub fn after_rpc_failure(failed_attempts: u32) -> Decision {
    Decision::RetryAfter(general_delay(failed_attempts))
}

pub fn after_rpc_status(_status: i32, _failed_attempts: u32) -> Decision {
    // A returned esp_err_t means the request reached the C6 and was refused.
    // Retrying an invalid state/configuration indefinitely hides the useful
    // distinction from a transient missing event.
    Decision::Stop
}

pub fn should_reset_after_stable(elapsed_ms: u64) -> bool {
    elapsed_ms >= STABLE_CONNECTION_MS
}

fn general_delay(failed_attempts: u32) -> u32 {
    let shift = failed_attempts.saturating_sub(1).min(5);
    (FIRST_GENERAL_MS << shift).min(MAX_GENERAL_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_four_gets_three_short_retries() {
        assert_eq!(after_disconnect(4, 1), Decision::RetryAfter(500));
        assert_eq!(after_disconnect(4, 3), Decision::RetryAfter(500));
        assert_eq!(after_disconnect(4, 4), Decision::RetryAfter(1_000));
    }

    #[test]
    fn general_backoff_saturates_at_thirty_seconds() {
        let expected = [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000];
        for (index, delay) in expected.iter().enumerate() {
            assert_eq!(
                after_disconnect(201, index as u32 + 1),
                Decision::RetryAfter(*delay)
            );
        }
    }

    #[test]
    fn authentication_failures_stop() {
        for reason in [15, 202, 204, 210, 211] {
            assert_eq!(after_disconnect(reason, 1), Decision::Stop);
        }
    }

    #[test]
    fn a_timeout_is_retryable() {
        assert_eq!(after_timeout(1), Decision::RetryAfter(1_000));
        assert_eq!(after_timeout(2), Decision::RetryAfter(2_000));
    }

    #[test]
    fn an_rpc_transport_failure_uses_the_general_backoff() {
        assert_eq!(after_rpc_failure(1), Decision::RetryAfter(1_000));
        assert_eq!(after_rpc_failure(6), Decision::RetryAfter(30_000));
    }

    #[test]
    fn ten_stable_minutes_reset_the_failure_streak() {
        assert!(!should_reset_after_stable(599_999));
        assert!(should_reset_after_stable(600_000));
    }
}
