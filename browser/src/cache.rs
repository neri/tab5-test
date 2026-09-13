//! The pure half of the browser's file-backed HTTP cache.
//!
//! Responses are kept as files on the RAM disk under [`CACHE_ROOT`];
//! `app::cache_store` owns the filesystem side. This module decides
//! everything that needs no filesystem: the key a response is kept under,
//! which files hold it, what its metadata file says, how long it stays
//! fresh, and in what order entries are given up.
//!
//! Times are milliseconds of uptime, not wall-clock time. The RTC may never
//! have been set, and `/tmp` is on the RAM disk every reset reformats, so
//! uptime is the one clock that is valid for as long as an entry can exist.

use alloc::string::String;

use crate::limits::{DEFAULT_CACHE_FRESHNESS_SECS, MAX_CACHE_META_BYTES};
use crate::memory::{self, OutOfMemory};
use crate::url::Url;

/// Where entries live: one directory per bucket, two files per entry.
///
/// ```text
/// /tmp/browser-cache/<b>/<hash>.body   the response body as transferred
/// /tmp/browser-cache/<b>/<hash>.meta   a Record, as text
/// ```
///
/// `<hash>` is the 64-bit FNV-1a of the key in 16 hex digits and `<b>` its
/// first digit, so no directory holds more than a sixteenth of the entries.
/// The metadata names the full key, so a hash collision is a miss rather
/// than the wrong page.
pub const CACHE_ROOT: &str = "/tmp/browser-cache";
pub const BUCKET_COUNT: u8 = 16;

const META_MAGIC: &str = "tab5-browser-cache 1";

/// The URL a response is kept under: everything but the fragment, which is
/// never sent and cannot change the response.
pub fn cache_key(url: &Url) -> Option<String> {
    url.without_fragment().ok()?.to_text().ok()
}

pub fn key_hash(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub fn bucket_path(bucket: u8) -> Result<String, OutOfMemory> {
    let mut path = memory::string_from(CACHE_ROOT)?;
    memory::push_char(&mut path, '/')?;
    memory::push_char(&mut path, hex_digit(bucket & 0x0f))?;
    Ok(path)
}

/// The files one entry occupies.
pub struct EntryPaths {
    pub hash: u64,
    pub bucket: u8,
    pub bucket_dir: String,
    pub body: String,
    pub meta: String,
}

pub fn entry_paths(key: &str) -> Result<EntryPaths, OutOfMemory> {
    entry_paths_for_hash(key_hash(key))
}

pub fn entry_paths_for_hash(hash: u64) -> Result<EntryPaths, OutOfMemory> {
    let bucket = (hash >> 60) as u8;
    let bucket_dir = bucket_path(bucket)?;
    let mut base = memory::string_from(&bucket_dir)?;
    memory::push_char(&mut base, '/')?;
    for shift in (0..16).rev() {
        memory::push_char(&mut base, hex_digit(((hash >> (shift * 4)) & 0x0f) as u8))?;
    }
    let mut body = memory::string_from(&base)?;
    memory::push_str(&mut body, ".body")?;
    memory::push_str(&mut base, ".meta")?;
    Ok(EntryPaths {
        hash,
        bucket,
        bucket_dir,
        body,
        meta: base,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    Body,
    Meta,
}

/// A file name inside a bucket directory, as its hash and which file it is.
/// Anything else is not one of this cache's files.
pub fn parse_entry_name(name: &str) -> Option<(u64, FileKind)> {
    let (stem, extension) = name.rsplit_once('.')?;
    let kind = if extension.eq_ignore_ascii_case("body") {
        FileKind::Body
    } else if extension.eq_ignore_ascii_case("meta") {
        FileKind::Meta
    } else {
        return None;
    };
    if stem.len() != 16 {
        return None;
    }
    Some((u64::from_str_radix(stem, 16).ok()?, kind))
}

fn hex_digit(value: u8) -> char {
    char::from(b"0123456789abcdef"[usize::from(value & 0x0f)])
}

/// What an entry's metadata file records.
#[derive(Debug, PartialEq, Eq)]
pub struct Record {
    pub key: String,
    /// `ETag` as sent; empty when there was none.
    pub etag: String,
    /// Lowercased media type without parameters; empty when none was sent.
    pub media_type: String,
    /// Empty when none was sent.
    pub charset: String,
    pub body_bytes: u64,
    pub stored_ms: u64,
    /// The uptime at which the entry stops being fresh and is purged.
    pub expires_ms: u64,
    /// The last time the entry was used, for least-recently-used purging.
    pub used_ms: u64,
    /// `Cache-Control: no-cache`: reused only after a `304`.
    pub revalidate: bool,
    /// What the connection the body came off proved, so a page shown from
    /// the cache keeps its padlock: 0 nothing (not from a network),
    /// 1 plaintext, 2 unauthenticated TLS, 3 pinned TLS.
    pub security: u8,
}

impl Record {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_ms
    }

    /// Whether the body can be shown without asking the server.
    pub fn reusable_without_request(&self, now_ms: u64) -> bool {
        !self.is_expired(now_ms) && !self.revalidate
    }

    /// The text of the metadata file. `None` when a value would break the
    /// line format or memory runs out.
    pub fn encode(&self) -> Option<String> {
        let texts = [&self.key, &self.etag, &self.media_type, &self.charset];
        if texts.iter().any(|text| text.contains(['\r', '\n'])) {
            return None;
        }
        let mut out = String::new();
        let mut line = |name: &str, value: &str| -> Result<(), OutOfMemory> {
            memory::push_str(&mut out, name)?;
            memory::push_char(&mut out, ' ')?;
            memory::push_str(&mut out, value)?;
            memory::push_char(&mut out, '\n')
        };
        let number = |value: u64| {
            let mut digits = [0u8; 20];
            let mut used = 0;
            let mut remaining = value;
            loop {
                digits[19 - used] = b'0' + (remaining % 10) as u8;
                used += 1;
                remaining /= 10;
                if remaining == 0 {
                    break;
                }
            }
            (digits, used)
        };
        let mut encoded = || -> Result<(), OutOfMemory> {
            line(
                META_MAGIC.split_once(' ').unwrap().0,
                META_MAGIC.split_once(' ').unwrap().1,
            )?;
            line("key", &self.key)?;
            line("etag", &self.etag)?;
            line("type", &self.media_type)?;
            line("charset", &self.charset)?;
            for (name, value) in [
                ("bytes", self.body_bytes),
                ("stored", self.stored_ms),
                ("expires", self.expires_ms),
                ("used", self.used_ms),
                ("revalidate", u64::from(self.revalidate)),
                ("security", u64::from(self.security)),
            ] {
                let (digits, used) = number(value);
                line(
                    name,
                    core::str::from_utf8(&digits[20 - used..]).unwrap_or("0"),
                )?;
            }
            Ok(())
        };
        encoded().ok()?;
        (out.len() <= MAX_CACHE_META_BYTES).then_some(out)
    }

    /// Reads a metadata file. `None` for anything not written by
    /// [`Record::encode`]: a missing field, a bad number, the wrong format
    /// version, or a file larger than one can be.
    pub fn decode(bytes: &[u8]) -> Option<Record> {
        if bytes.len() > MAX_CACHE_META_BYTES {
            return None;
        }
        let text = core::str::from_utf8(bytes).ok()?;
        let mut lines = text.split('\n');
        if lines.next()? != META_MAGIC {
            return None;
        }
        let mut key = None;
        let mut etag = None;
        let mut media_type = None;
        let mut charset = None;
        let mut numbers = [None::<u64>; 6];
        for line in lines.filter(|line| !line.is_empty()) {
            let (name, value) = line.split_once(' ')?;
            let number = |value: &str| {
                (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| value.parse::<u64>().ok())
                    .flatten()
            };
            match name {
                "key" => key = Some(memory::string_from(value).ok()?),
                "etag" => etag = Some(memory::string_from(value).ok()?),
                "type" => media_type = Some(memory::string_from(value).ok()?),
                "charset" => charset = Some(memory::string_from(value).ok()?),
                "bytes" => numbers[0] = Some(number(value)?),
                "stored" => numbers[1] = Some(number(value)?),
                "expires" => numbers[2] = Some(number(value)?),
                "used" => numbers[3] = Some(number(value)?),
                "revalidate" => numbers[4] = Some(number(value).filter(|flag| *flag <= 1)?),
                "security" => numbers[5] = Some(number(value).filter(|code| *code <= 3)?),
                _ => return None,
            }
        }
        let key = key.filter(|key| !key.is_empty())?;
        Some(Record {
            key,
            etag: etag?,
            media_type: media_type?,
            charset: charset?,
            body_bytes: numbers[0]?,
            stored_ms: numbers[1]?,
            expires_ms: numbers[2]?,
            used_ms: numbers[3]?,
            revalidate: numbers[4]? == 1,
            security: numbers[5]? as u8,
        })
    }
}

/// The headers a response's freshness is computed from.
#[derive(Clone, Copy, Default)]
pub struct Freshness<'a> {
    /// `Cache-Control: max-age`.
    pub max_age: Option<u64>,
    /// `Expires` as sent. Present but not a date means already expired.
    pub expires: Option<&'a [u8]>,
    pub date: Option<&'a [u8]>,
    pub age: Option<u64>,
}

/// How many seconds from now a response stays fresh, or `None` when it is
/// already stale and must not be kept.
///
/// `max-age` wins over `Expires`. `Expires` counts from the response's own
/// `Date`, because the board's clock may be unset; without a `Date` it is
/// ignored. With neither, the response gets
/// [`DEFAULT_CACHE_FRESHNESS_SECS`]. `Age` is subtracted from whichever
/// lifetime applies.
pub fn fresh_seconds(headers: Freshness<'_>) -> Option<u64> {
    let lifetime: i64 = match (headers.max_age, headers.expires) {
        (Some(seconds), _) => i64::try_from(seconds).unwrap_or(i64::MAX),
        (None, Some(expires)) => {
            let expires = parse_http_date(expires)?;
            match headers.date.and_then(parse_http_date) {
                Some(date) => expires.saturating_sub(date),
                None => DEFAULT_CACHE_FRESHNESS_SECS as i64,
            }
        }
        (None, None) => DEFAULT_CACHE_FRESHNESS_SECS as i64,
    };
    let age = i64::try_from(headers.age.unwrap_or(0)).unwrap_or(i64::MAX);
    let remaining = lifetime.saturating_sub(age);
    (remaining > 0).then_some(remaining as u64)
}

/// An IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`) as Unix seconds. The two
/// obsolete formats HTTP also allows are not read; `None` for them.
pub fn parse_http_date(text: &[u8]) -> Option<i64> {
    let text = core::str::from_utf8(text).ok()?.trim();
    let bytes = text.as_bytes();
    if bytes.len() != 29 || &bytes[3..5] != b", " || &bytes[25..] != b" GMT" {
        return None;
    }
    if !["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].contains(&&text[..3]) {
        return None;
    }
    let digits = |range: core::ops::Range<usize>| -> Option<i64> {
        let part = text.get(range)?;
        part.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| part.parse().ok())
            .flatten()
    };
    let day = digits(5..7)?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| *name == &text[8..11])? as i64
        + 1;
    let year = digits(12..16)?;
    if bytes[7] != b' '
        || bytes[11] != b' '
        || bytes[16] != b' '
        || bytes[19] != b':'
        || bytes[22] != b':'
    {
        return None;
    }
    let (hour, minute, second) = (digits(17..19)?, digits(20..22)?, digits(23..25)?);
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Days from the civil calendar, after Howard Hinnant's algorithm.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// One entry as purging sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub hash: u64,
    pub expires_ms: u64,
    pub used_ms: u64,
}

/// Orders entries the way they are given up: every expired one first, then
/// the least recently used.
pub fn sort_for_purge(candidates: &mut [Candidate], now_ms: u64) {
    candidates.sort_unstable_by_key(|candidate| {
        (
            candidate.expires_ms > now_ms,
            candidate.used_ms,
            candidate.hash,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            key: "http://h/p?q=1".into(),
            etag: "W/\"v1\"".into(),
            media_type: "text/html".into(),
            charset: String::new(),
            body_bytes: 1234,
            stored_ms: 10,
            expires_ms: 3_600_010,
            used_ms: 20,
            revalidate: true,
            security: 2,
        }
    }

    #[test]
    fn keys_ignore_the_fragment_and_name_stable_files() {
        let url = Url::parse("http://h/p?q=1#part").unwrap();
        let key = cache_key(&url).unwrap();
        assert_eq!(key, "http://h/p?q=1");
        let paths = entry_paths(&key).unwrap();
        assert_eq!(paths.hash, key_hash("http://h/p?q=1"));
        assert_eq!(paths.bucket, (paths.hash >> 60) as u8);
        assert!(paths.body.starts_with("/tmp/browser-cache/"));
        let name = paths.body.rsplit('/').next().unwrap();
        assert_eq!(parse_entry_name(name), Some((paths.hash, FileKind::Body)));
        let name = paths.meta.rsplit('/').next().unwrap();
        assert_eq!(parse_entry_name(name), Some((paths.hash, FileKind::Meta)));
        assert_eq!(
            &paths.bucket_dir[CACHE_ROOT.len()..],
            &alloc::format!("/{:x}", paths.bucket)
        );
        assert_eq!(parse_entry_name("0123.body"), None);
        assert_eq!(parse_entry_name("0123456789abcdef.txt"), None);
    }

    #[test]
    fn records_round_trip_and_reject_anything_else() {
        let encoded = record().encode().unwrap();
        assert_eq!(Record::decode(encoded.as_bytes()), Some(record()));
        assert_eq!(Record::decode(b"other 1\nkey x\n"), None);
        let missing = encoded.replace("used 20\n", "");
        assert_eq!(Record::decode(missing.as_bytes()), None);
        let bad = encoded.replace("bytes 1234", "bytes 12x");
        assert_eq!(Record::decode(bad.as_bytes()), None);
        let bad = encoded.replace("security 2", "security 4");
        assert_eq!(Record::decode(bad.as_bytes()), None);
        let mut newline = record();
        newline.etag = "a\nused 0".into();
        assert!(newline.encode().is_none());
        assert!(Record::decode(&alloc::vec![b'a'; MAX_CACHE_META_BYTES + 1]).is_none());
    }

    #[test]
    fn expiry_and_revalidation_decide_reuse() {
        let mut entry = record();
        assert!(!entry.reusable_without_request(100));
        entry.revalidate = false;
        assert!(entry.reusable_without_request(3_600_009));
        assert!(entry.is_expired(3_600_010));
        assert!(!entry.reusable_without_request(3_600_010));
    }

    #[test]
    fn http_dates_parse_only_in_the_fixed_format() {
        assert_eq!(
            parse_http_date(b"Sun, 06 Nov 1994 08:49:37 GMT"),
            Some(784_111_777)
        );
        assert_eq!(parse_http_date(b"Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_http_date(b"Sunday, 06-Nov-94 08:49:37 GMT"), None);
        assert_eq!(parse_http_date(b"0"), None);
        assert_eq!(parse_http_date(b"Sun, 06 Foo 1994 08:49:37 GMT"), None);
    }

    #[test]
    fn freshness_prefers_max_age_then_expires_then_the_default() {
        let date = b"Sun, 06 Nov 1994 08:49:37 GMT".as_slice();
        let later = b"Sun, 06 Nov 1994 08:50:37 GMT".as_slice();
        let fresh = |max_age, expires, date, age| {
            fresh_seconds(Freshness {
                max_age,
                expires,
                date,
                age,
            })
        };
        assert_eq!(fresh(Some(30), Some(later), Some(date), None), Some(30));
        assert_eq!(fresh(None, Some(later), Some(date), None), Some(60));
        assert_eq!(fresh(None, Some(later), Some(date), Some(15)), Some(45));
        assert_eq!(fresh(Some(10), None, None, Some(10)), None);
        assert_eq!(fresh(Some(0), None, None, None), None);
        assert_eq!(fresh(None, Some(b"0"), Some(date), None), None);
        assert_eq!(fresh(None, Some(date), Some(later), None), None);
        assert_eq!(
            fresh(None, Some(later), None, None),
            Some(DEFAULT_CACHE_FRESHNESS_SECS as u64)
        );
        assert_eq!(
            fresh(None, None, None, None),
            Some(DEFAULT_CACHE_FRESHNESS_SECS as u64)
        );
    }

    #[test]
    fn purging_takes_expired_entries_then_the_least_recently_used() {
        let mut entries = [
            Candidate {
                hash: 1,
                expires_ms: 500,
                used_ms: 90,
            },
            Candidate {
                hash: 2,
                expires_ms: 50,
                used_ms: 99,
            },
            Candidate {
                hash: 3,
                expires_ms: 500,
                used_ms: 10,
            },
            Candidate {
                hash: 4,
                expires_ms: 90,
                used_ms: 5,
            },
        ];
        sort_for_purge(&mut entries, 100);
        let order: alloc::vec::Vec<u64> = entries.iter().map(|entry| entry.hash).collect();
        assert_eq!(order, [4, 2, 3, 1]);
    }
}
