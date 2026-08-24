//! Absolute path parsing and normalization for the VFS.
//!
//! Paths are UTF-8 and always absolute. There is no current directory, so
//! there is nothing for a relative path to be relative to, and rejecting one
//! is more useful than inventing a root to resolve it against.
//!
//! Normalization happens once, here, at the edge. Everything below this --
//! the mount lookup, the filesystem driver, the name comparison -- works on
//! the canonical form, so no layer has to wonder whether it is looking at
//! `/ram//a/./b` or `/ram/a/b`, and `..` cannot be smuggled past a mount
//! point into another volume.
//!
//! The length limits are fixed here rather than taken from the filesystem:
//! FAT and exFAT disagree about them, and a path that is legal on one volume
//! and not on another would make the VFS's own answers depend on which
//! medium a caller happened to name.

/// Longest canonical path the VFS accepts, in bytes.
///
/// 255 is FAT's long-name limit for a single component, and using it for the
/// whole path keeps one number to remember. It is bytes rather than
/// characters because that is what bounds the storage; a path of multi-byte
/// characters holds fewer of them.
pub const MAX_PATH_BYTES: usize = 255;
/// Longest single component. Equal to the whole-path limit, so the binding
/// constraint is always the total rather than a second rule to check.
pub const MAX_COMPONENT_BYTES: usize = 255;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PathError {
    /// The path does not start with `/`.
    NotAbsolute,
    TooLong,
    ComponentTooLong,
    /// More `..` than there are components to remove. Refused rather than
    /// clamped at the root: a caller that wrote it meant somewhere else.
    EscapesRoot,
    /// A component contains a byte no volume this VFS mounts can name -- a
    /// control character, or one of the characters FAT reserves.
    InvalidCharacter,
}

pub fn error_name(error: PathError) -> &'static str {
    match error {
        PathError::NotAbsolute => "path must be absolute",
        PathError::TooLong => "path too long",
        PathError::ComponentTooLong => "path component too long",
        PathError::EscapesRoot => "path escapes the root",
        PathError::InvalidCharacter => "invalid character in path",
    }
}

/// A normalized absolute path: `/`, or `/a`, `/a/b`, with no trailing slash,
/// no empty components, and no `.` or `..` left in it.
#[derive(Clone, Copy)]
pub struct Path {
    buffer: [u8; MAX_PATH_BYTES],
    length: usize,
}

impl Path {
    pub fn as_str(&self) -> &str {
        // Every byte came from a `&str` and components are split on ASCII
        // `/`, which cannot fall inside a multi-byte character.
        core::str::from_utf8(&self.buffer[..self.length]).unwrap_or("/")
    }

    pub fn is_root(&self) -> bool {
        self.length == 1
    }

    /// The components, root yielding none.
    pub fn components(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.as_str().split('/').filter(|part| !part.is_empty())
    }

    /// The path with its last component removed, or `None` for the root.
    ///
    /// `/a` yields the root rather than `None`: the root is a real directory
    /// that a file can sit in, and returning `None` for it would make a
    /// caller creating `/ram/a` have to special-case the one case that is
    /// most common.
    pub fn parent(&self) -> Option<Path> {
        if self.is_root() {
            return None;
        }
        let text = self.as_str();
        // Every non-root path starts with `/` and has no trailing one, so
        // there is always a separator to cut at.
        let cut = text.rfind('/')?;
        if cut == 0 {
            return Some(root());
        }
        let mut buffer = [0u8; MAX_PATH_BYTES];
        buffer[..cut].copy_from_slice(&text.as_bytes()[..cut]);
        Some(Path {
            buffer,
            length: cut,
        })
    }

    /// The last component, or `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        self.components().next_back()
    }

    /// The part of this path below `prefix`, as a canonical path.
    ///
    /// Matching is by whole components, so `/ram` is a prefix of `/ram/a`
    /// but not of `/ramdisk` -- comparing raw bytes would make one mount
    /// point capture paths belonging to another.
    pub fn strip_prefix(&self, prefix: &Path) -> Option<Path> {
        if prefix.is_root() {
            return Some(*self);
        }
        let own = self.as_str();
        let prefix = prefix.as_str();
        if !own.starts_with(prefix) {
            return None;
        }
        match own.as_bytes().get(prefix.len()) {
            None => Some(root()),
            Some(b'/') => normalize(&own[prefix.len()..]).ok(),
            Some(_) => None,
        }
    }
}

pub fn root() -> Path {
    let mut buffer = [0u8; MAX_PATH_BYTES];
    buffer[0] = b'/';
    Path { buffer, length: 1 }
}

/// Parses and normalizes an absolute path.
pub fn normalize(input: &str) -> Result<Path, PathError> {
    if !input.starts_with('/') {
        return Err(PathError::NotAbsolute);
    }
    if input.len() > MAX_PATH_BYTES {
        return Err(PathError::TooLong);
    }

    // Component start offsets in `buffer`, so `..` can drop the last one by
    // truncating rather than by re-parsing what has been written.
    let mut starts = [0usize; MAX_PATH_BYTES / 2 + 1];
    let mut depth = 0usize;
    let mut buffer = [0u8; MAX_PATH_BYTES];
    let mut length = 0usize;

    for component in input.split('/') {
        match component {
            // Empty covers both a doubled slash and the trailing one.
            "" | "." => continue,
            ".." => {
                if depth == 0 {
                    return Err(PathError::EscapesRoot);
                }
                depth -= 1;
                length = starts[depth];
                continue;
            }
            _ => {}
        }
        if component.len() > MAX_COMPONENT_BYTES {
            return Err(PathError::ComponentTooLong);
        }
        if !is_nameable(component) {
            return Err(PathError::InvalidCharacter);
        }
        if length + 1 + component.len() > MAX_PATH_BYTES {
            return Err(PathError::TooLong);
        }
        starts[depth] = length;
        depth += 1;
        buffer[length] = b'/';
        length += 1;
        buffer[length..length + component.len()].copy_from_slice(component.as_bytes());
        length += component.len();
    }

    if length == 0 {
        return Ok(root());
    }
    Ok(Path { buffer, length })
}

/// Whether every character in a component can name a file on the volumes
/// this VFS mounts.
///
/// The excluded set is FAT's, which exFAT shares: control characters, and the
/// punctuation reserved for wildcards and path syntax. `/` never appears here
/// because the caller has already split on it.
fn is_nameable(component: &str) -> bool {
    !component
        .chars()
        .any(|character| (character as u32) < 0x20 || "\"*:<>?\\|".contains(character))
}

/// Compares two names the way FAT does: without regard to ASCII case.
///
/// Only ASCII case is folded. Case outside it is a per-codepage and,
/// for exFAT, a per-volume up-case table question, and guessing at it here
/// would give an answer that disagrees with the volume's own.
pub fn names_equal(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(&b))
}
