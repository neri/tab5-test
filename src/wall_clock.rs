//! What the RX8130CE's counters mean, and who is allowed to believe them.
//!
//! The device stores a calendar and nothing else -- no time zone, no epoch,
//! no statement of what the numbers are relative to. This file is where the
//! firmware decides: **the counters hold UTC**. `rtc set` takes UTC, the
//! registers hold UTC, and nothing converts on the way in or out of
//! [`crate::rtc`].
//!
//! Two different things then read that clock, and they are deliberately not
//! the same function.
//!
//! [`local_now`] is for people: the shell's `rtc` output and the FAT
//! timestamps a written file carries. It converts to
//! [`tab5_time::default_timezone`] -- JST, until something can change it --
//! and it does not care whether the clock is trustworthy, because a wrong
//! time on screen is a wrong time on screen and the caller can see the flags
//! beside it.
//!
//! [`unix_time_utc`] is for certificate validity checking, and it is far
//! stricter. It refuses unless the calendar reads, parses as a real date,
//! and the device itself reports that it has not lost power (`VLF`) and is
//! not being held stopped (`STOP`). A failure is "the time is not known",
//! and it stays that: nothing here substitutes the epoch, the build date or
//! a plausible-looking year, because a substituted time is what turns an
//! expired certificate into an accepted one.
//!
//! Nothing in the unauthenticated TLS profile or in SPKI pinning calls
//! [`unix_time_utc`]. Those check that the peer holds the key it presented,
//! which is a question with no date in it, so a Tab5 whose RTC has never
//! been set can still open an `https://` page. Only the public-CA profile
//! (`docs/TLS_PLAN.md` Stage 9) makes the clock a precondition, and that is
//! the whole reason the two readings are separate calls.

use tab5_time::{Calendar, TimeZone};

use crate::rtc;

/// Why the clock could not be believed.
///
/// The names are the failure names the browser and the shell report, so that
/// "the RTC was never set" and "the RTC answered with nonsense" stay
/// different answers all the way to the screen.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Error {
    /// The device did not answer, has lost power since it was last set
    /// (`VLF`), or is being held stopped (`STOP`). In every case the
    /// counters are not a time anyone set.
    Unset,
    /// The counters were readable but are not a date and time: bad BCD, a
    /// day past the end of its month, an hour past 23.
    Invalid,
}

impl Error {
    /// The one-word name fixtures match on.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unset => "clock-unset",
            Self::Invalid => "clock-invalid",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Unset => "the real-time clock is unreadable, stopped or never set",
            Self::Invalid => "the real-time clock is not holding a valid date",
        }
    }
}

/// The zone displays and FAT timestamps are written in.
///
/// One call rather than a constant added at each site, so that a stored
/// setting has one place to appear. Today it is always JST.
pub fn timezone() -> TimeZone {
    tab5_time::default_timezone()
}

/// The UTC calendar the device is holding, without judging the flags.
///
/// For display: a caller that also shows `VLF` and `STOP` is showing the
/// reader everything this knows, which is more useful than refusing.
pub fn utc_now() -> Result<Calendar, Error> {
    match rtc::read_datetime() {
        Ok(reading) if reading.is_valid() => Ok(reading.calendar()),
        Ok(_) => Err(Error::Invalid),
        Err(rtc::Error::InvalidTime) => Err(Error::Invalid),
        Err(_) => Err(Error::Unset),
    }
}

/// The same reading converted to [`timezone`].
///
/// The conversion can carry past 2099, which the device cannot store but a
/// display still has to print, so the result is a [`Calendar`] rather than
/// an [`rtc::DateTime`].
pub fn local_now() -> Result<Calendar, Error> {
    let utc = utc_now()?;
    tab5_time::local_datetime(utc, timezone()).ok_or(Error::Invalid)
}

/// Seconds since 1970-01-01T00:00:00Z, or why the clock cannot be believed.
///
/// The only reading certificate validity checking may use, and the only one
/// that consults [`rtc::Status`]. The three conditions are checked in one
/// pass over one pair of reads so that a caller cannot be handed a time from
/// before a `VLF` it has not seen yet.
pub fn unix_time_utc() -> Result<i64, Error> {
    let status = rtc::read_status().map_err(|_| Error::Unset)?;
    if status.voltage_low() || status.stopped() {
        return Err(Error::Unset);
    }
    let utc = utc_now()?;
    tab5_time::unix_time(utc).ok_or(Error::Invalid)
}
