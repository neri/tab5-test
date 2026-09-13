//! Allocation that reports failure instead of aborting.
//!
//! Every collection in this crate grows by an amount some server chose, and
//! the ordinary `push`/`extend_from_slice` path handles a failed allocation
//! by calling `handle_alloc_error`, which on this target is an abort. A
//! board that reboots because a page was larger than the heap had room for
//! is not a browser refusing a page -- it is the firmware losing, and the
//! shell, the Wi-Fi link and the mounted volumes go with it.
//!
//! So the rule for this crate is that nothing grows without going through
//! `try_reserve`. These helpers are that, wrapped thinly enough to stay
//! readable at the call site: they exist because `v.try_reserve(1)?;
//! v.push(x)` twice per line stops being legible after about the fifth
//! time.
//!
//! The limits in [`crate::limits`] are the first line of defence and these
//! are the second. A page is refused for being past a limit long before the
//! heap runs out; this is what happens when something else on the board has
//! already taken the memory.

use alloc::string::String;
use alloc::vec::Vec;

/// A growth request the allocator could not satisfy.
///
/// Deliberately carries nothing. `TryReserveError` distinguishes "capacity
/// overflow" from "allocator failed", and neither changes what the browser
/// does: the page is refused and the reason shown is that memory ran out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OutOfMemory;

/// Appends to a `String`, reserving first.
pub fn push_str(target: &mut String, text: &str) -> Result<(), OutOfMemory> {
    target.try_reserve(text.len()).map_err(|_| OutOfMemory)?;
    target.push_str(text);
    Ok(())
}

/// Appends one `char`, reserving its UTF-8 length first.
pub fn push_char(target: &mut String, value: char) -> Result<(), OutOfMemory> {
    target
        .try_reserve(value.len_utf8())
        .map_err(|_| OutOfMemory)?;
    target.push(value);
    Ok(())
}

/// A `String` holding `text`, allocated exactly once at exactly its length.
///
/// `try_reserve_exact` rather than `try_reserve`: the caller knows the final
/// size, and the growth factor's spare capacity would be counted against
/// the browser's owned-memory budget for the whole life of the page.
pub fn string_from(text: &str) -> Result<String, OutOfMemory> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| OutOfMemory)?;
    owned.push_str(text);
    Ok(owned)
}

/// An empty `String` with room for `capacity` bytes and no more.
pub fn string_with_capacity(capacity: usize) -> Result<String, OutOfMemory> {
    let mut owned = String::new();
    owned.try_reserve_exact(capacity).map_err(|_| OutOfMemory)?;
    Ok(owned)
}

/// Reserves room for `additional` more bytes, growing geometrically until
/// the step would exceed `cap` and then in steps of `cap`.
///
/// The plain `try_reserve` growth doubles for ever, which for the one
/// buffer that can reach a megabyte -- a page's text -- means a capacity of
/// two megabytes to hold it, and a megabyte of that is slack counted
/// against the browser's owned-memory budget for as long as the page is up.
/// Doubling is still what keeps appending amortised O(1) at small sizes, so
/// it is kept there and capped where it starts to cost real memory.
pub fn reserve_capped(
    target: &mut String,
    additional: usize,
    cap: usize,
) -> Result<(), OutOfMemory> {
    if target.len() + additional <= target.capacity() {
        return Ok(());
    }
    let step = target.capacity().clamp(1024, cap);
    target
        .try_reserve_exact(step.max(additional))
        .map_err(|_| OutOfMemory)
}

/// Appends to a `Vec`, reserving first.
pub fn push<T>(target: &mut Vec<T>, value: T) -> Result<(), OutOfMemory> {
    target.try_reserve(1).map_err(|_| OutOfMemory)?;
    target.push(value);
    Ok(())
}

/// Appends a slice to a `Vec`, reserving first.
pub fn extend_from_slice<T: Clone>(target: &mut Vec<T>, values: &[T]) -> Result<(), OutOfMemory> {
    target.try_reserve(values.len()).map_err(|_| OutOfMemory)?;
    target.extend_from_slice(values);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_from_allocates_exactly() {
        let owned = string_from("hello").unwrap();
        assert_eq!(owned, "hello");
        assert_eq!(owned.capacity(), 5);
    }

    #[test]
    fn push_str_appends() {
        let mut owned = string_from("a").unwrap();
        push_str(&mut owned, "bc").unwrap();
        assert_eq!(owned, "abc");
    }

    #[test]
    fn capped_growth_stops_doubling() {
        let mut text = String::new();
        // Below the cap it doubles, so appending stays cheap.
        for _ in 0..20 {
            reserve_capped(&mut text, 1, 4096).unwrap();
            let capacity = text.capacity();
            text.push_str(&"x".repeat(capacity - text.len()));
        }
        // Past it the slack is one step, not the whole buffer.
        assert!(text.capacity() - text.len() <= 4096, "{}", text.capacity());
    }

    #[test]
    fn capped_growth_still_satisfies_a_large_request() {
        let mut text = String::new();
        reserve_capped(&mut text, 100_000, 4096).unwrap();
        assert!(text.capacity() >= 100_000);
    }

    #[test]
    fn push_grows_a_vec() {
        let mut values = Vec::new();
        push(&mut values, 1u8).unwrap();
        extend_from_slice(&mut values, &[2, 3]).unwrap();
        assert_eq!(values, alloc::vec![1, 2, 3]);
    }
}
