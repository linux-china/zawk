//! Implementation of substring searches.
//!
//! This is a tiny wrapper on top of `memmem::find` from the `memchr` crate.
use super::str_impl::char_count;
use super::{Int, Str};
use memchr::memmem;

// The position in chars (as `length()` and `substr()` count), 1-indexed, 0 on failure.
pub fn index_substr<'a>(needle: &Str<'a>, haystack: &Str<'a>) -> Int {
    needle.with_bytes(|n| {
        haystack.with_bytes(|h| memmem::find(h, n).map_or(0, |x| char_count(&h[..x]) as Int + 1))
    })
}

pub fn last_index_substr<'a>(needle: &Str<'a>, haystack: &Str<'a>) -> Int {
    needle.with_bytes(|n| {
        haystack.with_bytes(|h| memmem::rfind(h, n).map_or(0, |x| char_count(&h[..x]) as Int + 1))
    })
}

