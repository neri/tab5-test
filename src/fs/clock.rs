//! The clock the filesystem writer stamps directory entries with.
//!
//! Two things make this less direct than handing the library a closure.
//!
//! The library takes a `&'static dyn TimeProvider`, so the provider cannot
//! carry a time captured for one operation. And reading the RX8130CE means
//! driving a software I2C bus, which is not something to do from inside a
//! directory-entry write -- the filesystem driver decides when it wants a
//! timestamp, and that can be several times while a single file is being
//! committed.
//!
//! So the RTC is sampled once per operation, before the write starts, into
//! a pair of atomics that the provider reads. Every entry written by one
//! operation carries the same timestamp, which is both cheaper and more
//! truthful than several readings that differ by however long the write
//! took.
//!
//! ## Which clock is written
//!
//! The RX8130CE holds UTC (`crate::wall_clock`), and FAT has nowhere to
//! record a time zone, so what goes into a directory entry is the local
//! reading -- JST, until something in the firmware can change it. That
//! matches what a PC writes to the same card, which is the point: a file
//! this firmware wrote and a file Windows wrote should not appear nine hours
//! apart in the same listing.
//!
//! ## When the clock is not set
//!
//! An unset or unreadable RTC writes zero into the date and time fields.
//! That is not a date -- it is not the FAT epoch and it is not the Unix
//! epoch -- it is the value FAT uses for "no timestamp", and every tool that
//! reads these volumes already understands it as absent. Writing 1980-01-01
//! instead would be a claim, and a false one.

use core::sync::atomic::{AtomicU16, Ordering};

use hadris_fat::time::{FatDateTime, TimeProvider};

use crate::{uart, wall_clock};

/// The FAT-encoded date and time the next entry write will use. Zero means
/// no timestamp; see this module's header.
static FAT_DATE: AtomicU16 = AtomicU16::new(0);
static FAT_TIME: AtomicU16 = AtomicU16::new(0);

/// Reads the RTC and holds the result for the operation about to run.
///
/// Called once at the start of a write, never from inside one. A failed or
/// invalid reading clears the stamp rather than leaving the previous one in
/// place: a stale timestamp on a new file is worse than none, because it
/// looks like a real answer.
pub fn sample() {
    let sampled = match wall_clock::local_now() {
        Ok(now) => Some(FatDateTime::new(
            now.year, now.month, now.day, now.hour, now.minute, now.second,
        )),
        Err(error) => {
            uart::log(b"FS: ");
            uart::log(error.message().as_bytes());
            uart::log(b"; writing no timestamp\r\n");
            None
        }
    };

    let (date, time) = match sampled {
        Some(stamp) => (stamp.date, stamp.time),
        None => (0, 0),
    };
    FAT_DATE.store(date, Ordering::Relaxed);
    FAT_TIME.store(time, Ordering::Relaxed);
}

/// Discards the sampled time, so an entry written outside a sampled
/// operation carries no timestamp instead of the last one taken.
pub fn clear() {
    FAT_DATE.store(0, Ordering::Relaxed);
    FAT_TIME.store(0, Ordering::Relaxed);
}

/// The provider handed to the filesystem library.
///
/// What it hands over is already local time: [`sample`] did the conversion
/// from the device's UTC once, for the whole operation. Nothing here
/// converts, because a provider that reached for a time zone would be
/// reaching for it several times inside one file commit.
#[derive(Debug)]
pub struct RtcTimeProvider;

impl TimeProvider for RtcTimeProvider {
    fn now(&self) -> FatDateTime {
        FatDateTime {
            date: FAT_DATE.load(Ordering::Relaxed),
            time: FAT_TIME.load(Ordering::Relaxed),
            // Left at zero even when the clock is set: the RX8130CE counts
            // whole seconds, so any value here would be invented.
            time_tenth: 0,
        }
    }
}

pub static PROVIDER: RtcTimeProvider = RtcTimeProvider;
