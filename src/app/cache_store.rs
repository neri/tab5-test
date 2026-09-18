//! The filesystem half of the browser's HTTP cache: reading, writing and
//! purging the files `browser::cache` names under `/tmp/browser-cache`.
//!
//! Every call takes the VFS and the devices for its own duration and keeps
//! nothing borrowed. Bodies are read and written a bounded amount per call,
//! like `localfile` reads, so a half-megabyte entry never holds the frame
//! loop; metadata files are a few hundred bytes and go in one call.
//!
//! Nothing here fails a page. An entry that cannot be read is a miss, and a
//! body that cannot be written is a page that is not kept. A full volume is
//! the one failure with a remedy: the write gives up expired entries, then
//! the least recently used one, and tries again.

use alloc::string::String;
use alloc::vec::Vec;

use crate::browser::cache::{
    self, BUCKET_COUNT, CACHE_ROOT, Candidate, EntryPaths, FileKind, Record,
};
use crate::browser::document::Parser;
use crate::browser::error;
use crate::browser::limits::{MAX_CACHE_META_BYTES, MAX_HTTP_CACHE_ENTRY_BYTES};
use crate::browser::url::Url;
use crate::fs::Devices;
use crate::fs::vfs::{EntryKind, FileHandle, FsError, OpenMode, Vfs};
use crate::net::tls::Authentication;

use super::fetch::{CacheMeta, Failure, Outcome, PageSecurity, Refresh};

/// Body bytes read per step: the same as a transfer, because the parser's
/// share of the work is the same.
const READ_STEP: usize = 16 * 1024;
/// Body bytes written per step. Larger than a read: a write to the RAM disk
/// is a copy with no parser behind it.
const WRITE_STEP: usize = 64 * 1024;

/// What `i` reports about the cache since the Browser opened.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    /// Pages and images shown from a fresh entry without a request.
    pub hits: usize,
    /// Entries a `304` confirmed and that were then read.
    pub revalidated: usize,
    pub stored: usize,
    /// Entries removed by expiry, a sweep, a full volume, or being unreadable.
    pub purged: usize,
    /// Entries removed because a POST changed what they described.
    pub invalidated: usize,
    /// Storable bodies that were not kept after all.
    pub not_kept: usize,
}

/// A kept entry: its record and where its files are.
pub struct Hit {
    pub record: Record,
    pub paths: EntryPaths,
}

pub fn security_code(security: Option<PageSecurity>) -> u8 {
    match security {
        None => 0,
        Some(PageSecurity::Cleartext) => 1,
        Some(PageSecurity::Tls(Authentication::Unverified)) => 2,
        Some(PageSecurity::Tls(Authentication::Pinned)) => 3,
    }
}

fn security_from_code(code: u8) -> Option<PageSecurity> {
    match code {
        1 => Some(PageSecurity::Cleartext),
        2 => Some(PageSecurity::Tls(Authentication::Unverified)),
        3 => Some(PageSecurity::Tls(Authentication::Pinned)),
        _ => None,
    }
}

/// The entry for `url`, if one is kept and has not expired. An expired entry
/// is removed on the way and counts as a miss.
pub fn lookup(
    vfs: &mut Vfs,
    devices: &mut Devices,
    url: &Url,
    now_ms: u64,
    stats: &mut Stats,
) -> Option<Hit> {
    let hit = find(vfs, devices, url)?;
    if hit.record.is_expired(now_ms) {
        remove_paths(vfs, devices, &hit.paths);
        stats.purged += 1;
        return None;
    }
    Some(hit)
}

/// The entry for `url` whatever its expiry, for a `304` that has just
/// confirmed it.
pub fn find(vfs: &mut Vfs, devices: &mut Devices, url: &Url) -> Option<Hit> {
    let key = cache::cache_key(url)?;
    let paths = cache::entry_paths(&key).ok()?;
    let record = read_record(vfs, devices, &paths.meta)?;
    // A different key under the same name is a hash collision: a miss, and
    // the next store for either URL replaces it.
    (record.key == key).then_some(Hit { record, paths })
}

/// Removes `url`'s entry. True when there was one.
pub fn remove(vfs: &mut Vfs, devices: &mut Devices, url: &Url) -> bool {
    let Some(key) = cache::cache_key(url) else {
        return false;
    };
    let Ok(paths) = cache::entry_paths(&key) else {
        return false;
    };
    remove_paths(vfs, devices, &paths)
}

pub fn remove_hit(vfs: &mut Vfs, devices: &mut Devices, hit: &Hit) {
    remove_paths(vfs, devices, &hit.paths);
}

/// The record first, so a body left behind has nothing vouching for it.
/// True when a record was removed.
fn remove_paths(vfs: &mut Vfs, devices: &mut Devices, paths: &EntryPaths) -> bool {
    let removed = vfs.remove_file(devices, &paths.meta).is_ok();
    let _ = vfs.remove_file(devices, &paths.body);
    removed
}

/// Rewrites an entry's metadata. False when it could not be written, which
/// at worst makes the entry look older or a miss next time.
pub fn update_record(vfs: &mut Vfs, devices: &mut Devices, hit: &Hit) -> bool {
    hit.record
        .encode()
        .is_some_and(|text| write_small(vfs, devices, &hit.paths.meta, text.as_bytes()).is_ok())
}

/// What a `304` said, applied to the entry it confirmed. A new lifetime
/// counts from now; without one the stored lifetime starts again.
pub fn refresh(record: &mut Record, refresh: Option<Refresh>, now_ms: u64) {
    let lifetime = record.expires_ms.saturating_sub(record.stored_ms);
    let fresh_ms = refresh
        .and_then(|refresh| refresh.fresh_seconds)
        .map_or(lifetime, |seconds| seconds.saturating_mul(1000));
    record.stored_ms = now_ms;
    record.expires_ms = now_ms.saturating_add(fresh_ms);
    record.used_ms = now_ms;
    if refresh.is_some_and(|refresh| refresh.revalidate) {
        record.revalidate = true;
    }
}

fn read_record(vfs: &mut Vfs, devices: &mut Devices, path: &str) -> Option<Record> {
    let metadata = vfs.metadata(devices, path).ok()?;
    if metadata.size > MAX_CACHE_META_BYTES as u64 {
        return None;
    }
    let handle = vfs.open(devices, path, OpenMode::Read).ok()?;
    let mut buffer = [0u8; MAX_CACHE_META_BYTES];
    let mut used = 0;
    let complete = loop {
        if used == buffer.len() {
            break true;
        }
        match vfs.read(devices, &handle, &mut buffer[used..]) {
            Ok(0) => break true,
            Ok(count) => used += count,
            Err(_) => break false,
        }
    };
    vfs.close(handle);
    complete.then(|| Record::decode(&buffer[..used])).flatten()
}

fn write_small(
    vfs: &mut Vfs,
    devices: &mut Devices,
    path: &str,
    bytes: &[u8],
) -> Result<(), FsError> {
    let handle = vfs.open(devices, path, OpenMode::Truncate)?;
    let mut written = 0;
    let result = loop {
        if written == bytes.len() {
            break Ok(());
        }
        match vfs.write(devices, &handle, &bytes[written..]) {
            Ok(0) => break Err(FsError::NoSpace),
            Ok(count) => written += count,
            Err(error) => break Err(error),
        }
    };
    vfs.close(handle);
    result
}

fn ensure_dir(vfs: &mut Vfs, devices: &mut Devices, path: &str) -> Result<(), FsError> {
    match vfs.create_dir(devices, path) {
        Ok(()) | Err(FsError::AlreadyExists) => Ok(()),
        Err(error) => Err(error),
    }
}

/// A body being read back: into a parser for a page, or whole for an image.
#[must_use = "a CacheRead owns a file handle and has to be closed"]
pub struct CacheRead {
    handle: FileHandle,
    target: Target,
    read: usize,
    size: usize,
    security: Option<PageSecurity>,
    revalidated: bool,
}

enum Target {
    Page(Parser),
    Image(Vec<u8>),
    Finished,
}

impl CacheRead {
    /// Opens `hit`'s body for `url`. `None` when the entry cannot serve this
    /// use -- an image entry asked for as a page, or the other way round -- or
    /// its body is not the size its record says, which the caller treats as
    /// an entry to drop.
    pub fn start(
        vfs: &mut Vfs,
        devices: &mut Devices,
        url: &Url,
        hit: &Hit,
        image: bool,
        revalidated: bool,
    ) -> Option<CacheRead> {
        let size = usize::try_from(hit.record.body_bytes)
            .ok()
            .filter(|size| *size <= MAX_HTTP_CACHE_ENTRY_BYTES)?;
        let media = hit.record.media_type.as_str();
        let target = if image {
            if !matches!(media, "image/png" | "image/jpeg" | "image/webp") {
                return None;
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size).ok()?;
            Target::Image(bytes)
        } else {
            let markup =
                media.is_empty() || media == "text/html" || media == "application/xhtml+xml";
            if !markup && !media.starts_with("text/") {
                return None;
            }
            let built = if markup {
                Parser::new(url.clone())
            } else {
                Parser::plain(url.clone())
            };
            let mut parser = built.ok()?;
            if !hit.record.charset.is_empty() {
                parser.declare_charset(hit.record.charset.as_bytes());
            }
            Target::Page(parser)
        };
        let metadata = vfs.metadata(devices, &hit.paths.body).ok()?;
        if metadata.size != size as u64 {
            return None;
        }
        let handle = vfs.open(devices, &hit.paths.body, OpenMode::Read).ok()?;
        Some(CacheRead {
            handle,
            target,
            read: 0,
            size,
            security: security_from_code(hit.record.security),
            revalidated,
        })
    }

    pub fn received(&self) -> usize {
        self.read
    }

    pub fn peak_owned(&self) -> usize {
        match &self.target {
            Target::Page(parser) => parser.owned_bytes(),
            _ => 0,
        }
    }

    /// What the connection the body originally came off proved.
    pub fn security(&self) -> Option<PageSecurity> {
        self.security
    }

    /// Whether a `304` confirmed this copy just now, rather than it being
    /// used while fresh.
    pub fn revalidated(&self) -> bool {
        self.revalidated
    }

    pub fn step(&mut self, vfs: &mut Vfs, devices: &mut Devices) -> Outcome {
        let mut buffer = [0u8; READ_STEP];
        let count = match vfs.read(devices, &self.handle, &mut buffer) {
            Ok(count) => count,
            Err(_) => return Outcome::Failed(UNREADABLE),
        };
        if count == 0 {
            return self.finish();
        }
        self.read += count;
        if self.read > self.size {
            return Outcome::Failed(UNREADABLE);
        }
        match &mut self.target {
            Target::Page(parser) => match parser.feed(&buffer[..count]) {
                Ok(()) => Outcome::Working,
                Err(failure) => Outcome::Failed(document_failure(failure)),
            },
            Target::Image(bytes) => {
                bytes.extend_from_slice(&buffer[..count]);
                Outcome::Working
            }
            Target::Finished => Outcome::Failed(UNREADABLE),
        }
    }

    fn finish(&mut self) -> Outcome {
        if self.read != self.size {
            return Outcome::Failed(UNREADABLE);
        }
        match core::mem::replace(&mut self.target, Target::Finished) {
            Target::Page(parser) => match parser.finish() {
                Ok(document) => Outcome::Page(document),
                Err(failure) => Outcome::Failed(document_failure(failure)),
            },
            Target::Image(bytes) if !bytes.is_empty() => Outcome::Image(bytes),
            _ => Outcome::Failed(UNREADABLE),
        }
    }

    pub fn close(self, vfs: &mut Vfs) {
        vfs.close(self.handle);
    }
}

/// A body on its way to the RAM disk, then its record.
#[must_use = "a CacheWrite may own a file handle and has to be finished or abandoned"]
pub struct CacheWrite {
    paths: EntryPaths,
    meta_text: String,
    body: Vec<u8>,
    handle: Option<FileHandle>,
    written: usize,
}

pub enum WriteProgress {
    Working,
    Stored,
    NotKept,
}

impl CacheWrite {
    /// Prepares an entry for `url`. `None` for a body past an entry or a
    /// record that cannot be written down.
    pub fn new(
        url: &Url,
        meta: CacheMeta,
        body: Vec<u8>,
        security: Option<PageSecurity>,
        now_ms: u64,
    ) -> Option<CacheWrite> {
        if body.len() > MAX_HTTP_CACHE_ENTRY_BYTES {
            return None;
        }
        let key = cache::cache_key(url)?;
        let paths = cache::entry_paths(&key).ok()?;
        let record = Record {
            key,
            etag: meta.etag,
            media_type: meta.media_type,
            charset: meta.charset,
            body_bytes: body.len() as u64,
            stored_ms: now_ms,
            expires_ms: now_ms.saturating_add(meta.fresh_seconds.saturating_mul(1000)),
            used_ms: now_ms,
            revalidate: meta.revalidate,
            security: security_code(security),
        };
        let meta_text = record.encode()?;
        Some(CacheWrite {
            paths,
            meta_text,
            body,
            handle: None,
            written: 0,
        })
    }

    pub fn step(
        &mut self,
        vfs: &mut Vfs,
        devices: &mut Devices,
        now_ms: u64,
        stats: &mut Stats,
    ) -> WriteProgress {
        match self.try_step(vfs, devices) {
            Ok(false) => WriteProgress::Working,
            Ok(true) => {
                stats.stored += 1;
                WriteProgress::Stored
            }
            Err(FsError::NoSpace) => {
                self.reset(vfs, devices);
                let removed = purge_for_space(vfs, devices, now_ms, self.paths.hash);
                stats.purged += removed;
                if removed == 0 {
                    stats.not_kept += 1;
                    WriteProgress::NotKept
                } else {
                    WriteProgress::Working
                }
            }
            Err(_) => {
                self.reset(vfs, devices);
                stats.not_kept += 1;
                WriteProgress::NotKept
            }
        }
    }

    /// One step. `Ok(true)` once the record is written.
    fn try_step(&mut self, vfs: &mut Vfs, devices: &mut Devices) -> Result<bool, FsError> {
        let Some(handle) = self.handle.as_ref() else {
            ensure_dir(vfs, devices, CACHE_ROOT)?;
            ensure_dir(vfs, devices, &self.paths.bucket_dir)?;
            // The old record goes before the body is replaced, so a body
            // half-written never has a record vouching for it.
            match vfs.remove_file(devices, &self.paths.meta) {
                Ok(()) | Err(FsError::NotFound) => {}
                Err(error) => return Err(error),
            }
            self.handle = Some(vfs.open(devices, &self.paths.body, OpenMode::Truncate)?);
            self.written = 0;
            return Ok(false);
        };
        if self.written < self.body.len() {
            let end = (self.written + WRITE_STEP).min(self.body.len());
            let count = vfs.write(devices, handle, &self.body[self.written..end])?;
            if count == 0 {
                return Err(FsError::NoSpace);
            }
            self.written += count;
            return Ok(false);
        }
        if let Some(handle) = self.handle.take() {
            vfs.close(handle);
        }
        write_small(vfs, devices, &self.paths.meta, self.meta_text.as_bytes())?;
        Ok(true)
    }

    /// Back to nothing written: no handle, no body, no record.
    fn reset(&mut self, vfs: &mut Vfs, devices: &mut Devices) {
        if let Some(handle) = self.handle.take() {
            vfs.close(handle);
        }
        remove_paths(vfs, devices, &self.paths);
        self.written = 0;
    }

    /// Whether this is the entry for `url`.
    pub fn is_for(&self, url: &Url) -> bool {
        cache::cache_key(url).is_some_and(|key| cache::key_hash(&key) == self.paths.hash)
    }

    /// Gives the handle back without finishing. A partial body has no record
    /// and the next sweep removes it.
    pub fn abandon(mut self, vfs: &mut Vfs) {
        if let Some(handle) = self.handle.take() {
            vfs.close(handle);
        }
    }
}

struct Scan {
    candidates: Vec<Candidate>,
    removed: usize,
}

/// Reads one bucket. Every complete entry becomes a candidate; every stray
/// file -- a body with no record, a record with no body, one that cannot be
/// read or names another key -- is removed. `keep` is an entry being written,
/// whose body has no record yet.
fn scan_bucket(vfs: &mut Vfs, devices: &mut Devices, bucket: u8, keep: Option<u64>) -> Scan {
    let mut scan = Scan {
        candidates: Vec::new(),
        removed: 0,
    };
    let Ok(dir) = cache::bucket_path(bucket) else {
        return scan;
    };
    let mut files: Vec<(u64, FileKind)> = Vec::new();
    let mut complete = true;
    let listed = vfs.list(devices, &dir, |entry| {
        if entry.kind != EntryKind::File {
            return;
        }
        if let Some(file) = cache::parse_entry_name(entry.name) {
            if files.try_reserve(1).is_ok() {
                files.push(file);
            } else {
                complete = false;
            }
        }
    });
    // Removing what looks stray from a partial listing could remove half of
    // an entry whose other half was simply not seen.
    if listed.is_err() || !complete {
        return scan;
    }
    for &(hash, kind) in &files {
        if Some(hash) == keep {
            continue;
        }
        let has = |wanted: FileKind| files.iter().any(|&(other, k)| other == hash && k == wanted);
        let Ok(paths) = cache::entry_paths_for_hash(hash) else {
            continue;
        };
        match kind {
            FileKind::Body => {
                if !has(FileKind::Meta) {
                    let _ = vfs.remove_file(devices, &paths.body);
                    scan.removed += 1;
                }
            }
            FileKind::Meta => {
                let record = read_record(vfs, devices, &paths.meta)
                    .filter(|record| cache::key_hash(&record.key) == hash);
                match record {
                    Some(record) if has(FileKind::Body) => {
                        if scan.candidates.try_reserve(1).is_ok() {
                            scan.candidates.push(Candidate {
                                hash,
                                expires_ms: record.expires_ms,
                                used_ms: record.used_ms,
                            });
                        }
                    }
                    _ => {
                        remove_paths(vfs, devices, &paths);
                        scan.removed += 1;
                    }
                }
            }
        }
    }
    scan
}

/// One step of the periodic expiry sweep: the expired entries and stray files
/// of one bucket. Returns how many were removed.
pub fn sweep_bucket(
    vfs: &mut Vfs,
    devices: &mut Devices,
    bucket: u8,
    now_ms: u64,
    keep: Option<u64>,
) -> usize {
    let scan = scan_bucket(vfs, devices, bucket, keep);
    let mut removed = scan.removed;
    for candidate in scan
        .candidates
        .iter()
        .filter(|candidate| candidate.expires_ms <= now_ms)
    {
        if let Ok(paths) = cache::entry_paths_for_hash(candidate.hash) {
            remove_paths(vfs, devices, &paths);
            removed += 1;
        }
    }
    removed
}

/// Makes room after a write found the volume full, one whole entry at a time.
///
/// Every expired entry and stray file goes. When there were none, the least
/// recently used entry goes instead -- one, because the caller retries and a
/// single removal is often enough. `keep` is the entry being written.
/// Returns how many entries and stray files were removed; zero means there
/// is nothing left to give up.
pub fn purge_for_space(vfs: &mut Vfs, devices: &mut Devices, now_ms: u64, keep: u64) -> usize {
    let mut candidates = Vec::new();
    let mut removed = 0;
    for bucket in 0..BUCKET_COUNT {
        let mut scan = scan_bucket(vfs, devices, bucket, Some(keep));
        removed += scan.removed;
        if candidates.try_reserve(scan.candidates.len()).is_ok() {
            candidates.append(&mut scan.candidates);
        }
    }
    cache::sort_for_purge(&mut candidates, now_ms);
    let expired = candidates
        .iter()
        .take_while(|candidate| candidate.expires_ms <= now_ms)
        .count();
    let chosen = if expired > 0 || removed > 0 {
        expired
    } else {
        candidates.len().min(1)
    };
    for candidate in &candidates[..chosen] {
        if let Ok(paths) = cache::entry_paths_for_hash(candidate.hash) {
            remove_paths(vfs, devices, &paths);
            removed += 1;
        }
    }
    removed
}

fn document_failure(failure: error::Error) -> Failure {
    Failure {
        name: error::error_name(failure),
        headline: "Cannot show this page",
        detail: error::error_text(failure),
        status: None,
    }
}

pub const UNREADABLE: Failure = Failure::new(
    "cache-read",
    "Cannot read the cached copy",
    "The cache file ended early or could not be read.",
);
