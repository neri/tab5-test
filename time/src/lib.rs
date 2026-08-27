//! Calendar arithmetic, Unix time, and fixed-offset time zones.
//!
//! Two clocks are kept apart here on purpose.
//!
//! What the RX8130CE holds is **UTC**. Nothing in this crate adds an offset
//! to it unless a caller names a [`TimeZone`], because the one consumer that
//! must never see a local time is certificate validity checking: an offset
//! quietly folded into a Unix second is a certificate accepted or rejected
//! nine hours away from where it should have been.
//!
//! What a person reads on the screen, and what a FAT directory entry
//! records, is **local time** -- [`JST`] until something in the firmware
//! learns how to change it. That conversion goes through
//! [`local_datetime`] rather than through a `+ 9` somewhere, so there is one
//! place to change when it does.
//!
//! The conversions are the civil-calendar pair from the proleptic Gregorian
//! calendar: [`days_from_civil`] and its inverse. They are exact over the
//! whole range this supports rather than only over the RX8130CE's 2000-2099,
//! which matters at the ends -- 2099-12-31 23:00 UTC is 2100-01-01 08:00 in
//! JST, a year the device cannot store but the display still has to print.

#![cfg_attr(not(test), no_std)]

/// One reading of a calendar: a date and a time of day, with no time zone
/// attached.
///
/// Whether an instance holds UTC or a local time is the caller's to track;
/// the type deliberately does not say, because a field claiming "this is
/// UTC" would be a claim no arithmetic here could check. What keeps the two
/// apart is the function names -- [`unix_time`] takes UTC, and
/// [`local_datetime`] is the only thing that produces a local reading.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct Calendar {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// The years [`unix_time`] and [`from_unix`] round-trip.
///
/// The lower end is the Unix epoch, so that no conversion here has to
/// represent a negative second. The upper end is past anything a
/// two-digit-year RTC can produce and leaves the arithmetic far from any
/// 32-bit edge.
pub const FIRST_YEAR: u16 = 1970;
pub const LAST_YEAR: u16 = 2999;

impl Calendar {
    /// Whether every field is in range for the calendar it describes,
    /// including the length of the given month in the given year.
    ///
    /// Leap seconds are not accepted: a `second` of 60 is a value no
    /// consumer of this crate can do anything with, and Unix time has no
    /// number for it either.
    pub fn is_valid(&self) -> bool {
        (FIRST_YEAR..=LAST_YEAR).contains(&self.year)
            && (1..=12).contains(&self.month)
            && self.day >= 1
            && self.day <= days_in_month(self.year, self.month)
            && self.hour <= 23
            && self.minute <= 59
            && self.second <= 59
    }

    /// Day of the week, 0 = Sunday.
    pub fn weekday(&self) -> Option<u8> {
        self.is_valid()
            .then(|| weekday_from_date(self.year, self.month, self.day))
            .flatten()
    }
}

/// A fixed offset from UTC, with the name to print beside it.
///
/// Fixed is all this is: there is no rule table, no daylight saving and no
/// history of past offsets, because the firmware has no way to choose a zone
/// yet and inventing storage for one here would be inventing the part that
/// is still undecided. When a zone becomes selectable, what changes is
/// [`default_timezone`]'s source -- not the callers, which already ask for a
/// zone rather than adding a constant.
#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub struct TimeZone {
    pub name: &'static str,
    /// Minutes to add to UTC. East of Greenwich is positive.
    pub offset_minutes: i16,
}

/// Japan Standard Time: the display and FAT-timestamp zone this firmware
/// uses, and the only one it can currently be in.
pub const JST: TimeZone = TimeZone {
    name: "JST",
    offset_minutes: 540,
};

/// The zone displays and FAT timestamps are converted into.
///
/// Constant today. The one place to change when a stored setting exists.
pub fn default_timezone() -> TimeZone {
    JST
}

impl TimeZone {
    /// The offset as `+HHMM` / `-HHMM` digits, for printing beside a local
    /// time. Returned as bytes so that a caller without a formatter can
    /// still write it.
    pub fn offset_text(&self) -> [u8; 5] {
        let (sign, magnitude) = if self.offset_minutes < 0 {
            (b'-', (-self.offset_minutes) as u16)
        } else {
            (b'+', self.offset_minutes as u16)
        };
        let (hours, minutes) = (magnitude / 60, magnitude % 60);
        [
            sign,
            b'0' + (hours / 10) as u8,
            b'0' + (hours % 10) as u8,
            b'0' + (minutes / 10) as u8,
            b'0' + (minutes % 10) as u8,
        ]
    }
}

pub fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

pub fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

/// Day of the week for a date, 0 = Sunday.
///
/// `None` for a date that does not exist, so that a caller cannot get a
/// plausible weekday out of an impossible day.
pub fn weekday_from_date(year: u16, month: u8, day: u8) -> Option<u8> {
    let days = days_from_civil(year, month, day)?;
    // 1970-01-01 was a Thursday, which is 4 counting from Sunday.
    Some(((days + 4).rem_euclid(7)) as u8)
}

/// Days from 1970-01-01 to the given date.
///
/// The shifted-year form of the proleptic Gregorian calendar: March is
/// treated as the first month, which puts the leap day at the end of the
/// year and removes every special case from the arithmetic below.
pub fn days_from_civil(year: u16, month: u8, day: u8) -> Option<i64> {
    if !(FIRST_YEAR..=LAST_YEAR).contains(&year)
        || !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
    {
        return None;
    }
    let year = i64::from(year) - i64::from(month <= 2);
    let era = year / 400;
    let year_of_era = year - era * 400; // 0..=399
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    // 719468 is the number of days from 0000-03-01 to 1970-01-01.
    Some(era * 146097 + day_of_era - 719468)
}

/// The inverse of [`days_from_civil`].
pub fn civil_from_days(days: i64) -> Option<(u16, u8, u8)> {
    let shifted = days + 719468;
    let era = shifted.div_euclid(146097);
    let day_of_era = shifted.rem_euclid(146097); // 0..=146096
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153; // 0 = March
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u8;
    let month = (shifted_month + if shifted_month < 10 { 3 } else { -9 }) as u8;
    let year = year + i64::from(month <= 2);
    let year = u16::try_from(year).ok()?;
    (FIRST_YEAR..=LAST_YEAR)
        .contains(&year)
        .then_some((year, month, day))
}

/// Seconds since 1970-01-01T00:00:00Z for a UTC calendar reading.
///
/// `None` for a reading that is not a real date and time. This is the only
/// function certificate validity checking may use, and it takes UTC by
/// contract: nothing here adds [`default_timezone`]'s offset.
pub fn unix_time(utc: Calendar) -> Option<i64> {
    if !utc.is_valid() {
        return None;
    }
    let days = days_from_civil(utc.year, utc.month, utc.day)?;
    Some(
        days * 86400
            + i64::from(utc.hour) * 3600
            + i64::from(utc.minute) * 60
            + i64::from(utc.second),
    )
}

/// The inverse of [`unix_time`]: the UTC calendar reading for a Unix second.
pub fn from_unix(seconds: i64) -> Option<Calendar> {
    let days = seconds.div_euclid(86400);
    let within_day = seconds.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days)?;
    Some(Calendar {
        year,
        month,
        day,
        hour: (within_day / 3600) as u8,
        minute: (within_day % 3600 / 60) as u8,
        second: (within_day % 60) as u8,
    })
}

/// The local reading of a UTC one, in the given zone.
///
/// The whole conversion is one addition on the Unix second and a conversion
/// back, so a date that crosses midnight, a month end, a leap day or a year
/// end carries exactly the way the calendar does -- there is no separate
/// path for those cases to be wrong in.
pub fn local_datetime(utc: Calendar, zone: TimeZone) -> Option<Calendar> {
    from_unix(unix_time(utc)? + i64::from(zone.offset_minutes) * 60)
}

/// The UTC reading of a local one -- what a person typing a local time into
/// a setting command would mean.
///
/// Not used by the firmware today, because `rtc set` takes UTC and says so.
/// It is here so that the inverse exists in the same place as the forward
/// conversion when a zone becomes selectable.
pub fn utc_datetime(local: Calendar, zone: TimeZone) -> Option<Calendar> {
    from_unix(unix_time(local)? - i64::from(zone.offset_minutes) * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Calendar {
        Calendar {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn epoch_is_zero() {
        assert_eq!(unix_time(calendar(1970, 1, 1, 0, 0, 0)), Some(0));
        assert_eq!(from_unix(0), Some(calendar(1970, 1, 1, 0, 0, 0)));
    }

    #[test]
    fn known_instants() {
        // Values a `date -u -d ... +%s` agrees with.
        assert_eq!(unix_time(calendar(2000, 1, 1, 0, 0, 0)), Some(946_684_800));
        assert_eq!(unix_time(calendar(2026, 8, 27, 3, 0, 0)), Some(1_787_799_600));
        assert_eq!(
            unix_time(calendar(2099, 12, 31, 23, 59, 59)),
            Some(4_102_444_799)
        );
    }

    /// Every second of both RX8130CE end years, and of every month end and
    /// leap day between, has to survive calendar -> Unix -> calendar.
    #[test]
    fn round_trips_across_the_device_range() {
        for year in 2000..=2099u16 {
            for month in 1..=12u8 {
                for day in [1, days_in_month(year, month)] {
                    for (hour, minute, second) in [(0, 0, 0), (12, 34, 56), (23, 59, 59)] {
                        let reading = calendar(year, month, day, hour, minute, second);
                        let seconds = unix_time(reading).expect("a real date");
                        assert_eq!(from_unix(seconds), Some(reading), "{reading:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn every_day_of_a_leap_year_and_a_common_one() {
        for year in [2000u16, 2024, 2100, 2400] {
            let mut days = 0;
            for month in 1..=12u8 {
                for day in 1..=days_in_month(year, month) {
                    let reading = calendar(year, month, day, 0, 0, 0);
                    assert_eq!(from_unix(unix_time(reading).unwrap()), Some(reading));
                    days += 1;
                }
            }
            assert_eq!(days, if is_leap_year(year) { 366 } else { 365 }, "{year}");
        }
    }

    #[test]
    fn leap_year_rule_at_the_century_boundaries() {
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(2100));
        assert!(is_leap_year(2400));
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2100, 2), 28);
        assert!(calendar(2000, 2, 29, 0, 0, 0).is_valid());
        assert!(!calendar(2100, 2, 29, 0, 0, 0).is_valid());
    }

    #[test]
    fn invalid_readings_have_no_unix_time() {
        for reading in [
            calendar(2026, 2, 30, 0, 0, 0),
            calendar(2026, 0, 1, 0, 0, 0),
            calendar(2026, 13, 1, 0, 0, 0),
            calendar(2026, 1, 0, 0, 0, 0),
            calendar(2026, 1, 1, 24, 0, 0),
            calendar(2026, 1, 1, 0, 60, 0),
            // A leap second: real in UTC, absent from Unix time, and not
            // something the RX8130CE can hold either.
            calendar(2026, 1, 1, 23, 59, 60),
            calendar(1969, 12, 31, 23, 59, 59),
        ] {
            assert_eq!(unix_time(reading), None, "{reading:?}");
        }
    }

    #[test]
    fn jst_is_nine_hours_and_never_reaches_unix_time() {
        assert_eq!(default_timezone(), JST);
        assert_eq!(JST.offset_minutes, 540);
        assert_eq!(&JST.offset_text(), b"+0900");

        let utc = calendar(2026, 8, 27, 3, 0, 0);
        assert_eq!(
            local_datetime(utc, JST),
            Some(calendar(2026, 8, 27, 12, 0, 0))
        );
        // The Unix second of the reading is the same number whichever zone
        // is being displayed; converting for display must not move it.
        let before = unix_time(utc);
        let _ = local_datetime(utc, JST);
        assert_eq!(unix_time(utc), before);
        assert_eq!(before, Some(1_787_799_600));
    }

    #[test]
    fn local_conversion_carries_across_every_boundary() {
        // Into the next day, month, and year.
        assert_eq!(
            local_datetime(calendar(2026, 8, 27, 15, 30, 0), JST),
            Some(calendar(2026, 8, 28, 0, 30, 0))
        );
        assert_eq!(
            local_datetime(calendar(2026, 8, 31, 20, 0, 0), JST),
            Some(calendar(2026, 9, 1, 5, 0, 0))
        );
        assert_eq!(
            local_datetime(calendar(2026, 12, 31, 23, 59, 59), JST),
            Some(calendar(2027, 1, 1, 8, 59, 59))
        );
        // Onto and off a leap day.
        assert_eq!(
            local_datetime(calendar(2000, 2, 28, 23, 0, 0), JST),
            Some(calendar(2000, 2, 29, 8, 0, 0))
        );
        assert_eq!(
            local_datetime(calendar(2100, 2, 28, 23, 0, 0), JST),
            Some(calendar(2100, 3, 1, 8, 0, 0))
        );
        // Past the last year the RX8130CE can store: the device cannot hold
        // 2100, but the display still has to print it rather than wrapping.
        assert_eq!(
            local_datetime(calendar(2099, 12, 31, 23, 0, 0), JST),
            Some(calendar(2100, 1, 1, 8, 0, 0))
        );
        // And the first instant the device can hold, seen from a zone west
        // of Greenwich, is the year before.
        let hawaii = TimeZone {
            name: "HST",
            offset_minutes: -600,
        };
        assert_eq!(&hawaii.offset_text(), b"-1000");
        assert_eq!(
            local_datetime(calendar(2000, 1, 1, 0, 0, 0), hawaii),
            Some(calendar(1999, 12, 31, 14, 0, 0))
        );
    }

    #[test]
    fn local_and_utc_conversions_are_inverses() {
        for zone in [
            JST,
            TimeZone {
                name: "UTC",
                offset_minutes: 0,
            },
            TimeZone {
                name: "NPT",
                offset_minutes: 345,
            },
            TimeZone {
                name: "HST",
                offset_minutes: -600,
            },
        ] {
            for reading in [
                calendar(2000, 1, 1, 12, 0, 0),
                calendar(2026, 8, 27, 3, 0, 0),
                calendar(2099, 12, 31, 0, 0, 1),
                calendar(2024, 2, 29, 23, 59, 59),
            ] {
                let local = local_datetime(reading, zone).expect("a real date");
                assert_eq!(utc_datetime(local, zone), Some(reading), "{zone:?}");
            }
        }
    }

    #[test]
    fn weekdays_match_known_dates() {
        // 2000-01-01 was a Saturday, 2026-08-27 a Thursday, 2099-12-31 a
        // Thursday, and 1970-01-01 a Thursday.
        assert_eq!(weekday_from_date(1970, 1, 1), Some(4));
        assert_eq!(weekday_from_date(2000, 1, 1), Some(6));
        assert_eq!(weekday_from_date(2026, 8, 27), Some(4));
        assert_eq!(weekday_from_date(2099, 12, 31), Some(4));
        assert_eq!(weekday_from_date(2026, 2, 30), None);
    }

    /// The weekday has to advance by exactly one per day with no gaps, which
    /// is what a table-free calculation can get wrong at a month or year end.
    #[test]
    fn weekdays_advance_one_per_day() {
        let mut expected = weekday_from_date(2000, 1, 1).unwrap();
        for day in 0..36_524i64 {
            let (year, month, date) = civil_from_days(days_from_civil(2000, 1, 1).unwrap() + day)
                .expect("inside the range");
            assert_eq!(weekday_from_date(year, month, date), Some(expected));
            expected = (expected + 1) % 7;
        }
    }

    #[test]
    fn the_supported_range_has_hard_ends() {
        assert_eq!(from_unix(unix_time(calendar(1970, 1, 1, 0, 0, 0)).unwrap() - 1), None);
        assert_eq!(civil_from_days(days_from_civil(LAST_YEAR, 12, 31).unwrap() + 1), None);
        assert_eq!(days_from_civil(FIRST_YEAR - 1, 12, 31), None);
        assert_eq!(days_from_civil(LAST_YEAR + 1, 1, 1), None);
    }
}
