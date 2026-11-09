//! Implementation of substring searches.
//!
//! This is a tiny wrapper on top of `memmem::find` from the `memchr` crate.
use super::{Int, Str};
use memchr::memmem;
use std::cell::RefCell;

thread_local! {
    /// The `Finder` for the most recent `index` needle. Building a `Finder` (with its SIMD
    /// prefilter) dominates `index(s, "const")` on short haystacks, and the needle is almost always
    /// the same from one call to the next, so we keep the last one around.
    static LAST_FINDER: RefCell<Option<memmem::Finder<'static>>> = const { RefCell::new(None) };
}

fn find_cached(h: &[u8], n: &[u8]) -> Option<usize> {
    LAST_FINDER.with(|cell| {
        let mut slot = cell.borrow_mut();
        match &*slot {
            Some(f) if f.needle() == n => {}
            _ => *slot = Some(memmem::Finder::new(n).into_owned()),
        }
        slot.as_ref().unwrap().find(h)
    })
}

// The position in chars (as `length()` and `substr()` count), 1-indexed, 0 on failure. Case is
// ignored when IGNORECASE is set.
pub fn index_substr<'a>(needle: &Str<'a>, haystack: &Str<'a>) -> Int {
    needle.with_bytes(|n| {
        haystack.with_bytes(|h| {
            super::compare::index_folded(n, h).unwrap_or_else(|| {
                find_cached(h, n).map_or(0, |x| haystack.chars_before(h, x) as Int + 1)
            })
        })
    })
}

pub fn last_index_substr<'a>(needle: &Str<'a>, haystack: &Str<'a>) -> Int {
    needle.with_bytes(|n| {
        haystack.with_bytes(|h| {
            memmem::rfind(h, n).map_or(0, |x| haystack.chars_before(h, x) as Int + 1)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_reuses_and_replaces_cached_finder() {
        let s = |x: &'static str| Str::from(x);
        // Same needle twice (cache hit), then a different needle (cache replaced), then back.
        assert_eq!(index_substr(&s("b"), &s("a-b-c")), 3);
        assert_eq!(index_substr(&s("b"), &s("bbb")), 1);
        assert_eq!(index_substr(&s("c"), &s("a-b-c")), 5);
        assert_eq!(index_substr(&s("b"), &s("a-b-c")), 3);
        assert_eq!(index_substr(&s("wö"), &s("héllo wörld")), 7);
        assert_eq!(index_substr(&s("x"), &s("abc")), 0);
        assert_eq!(index_substr(&s(""), &s("abc")), 1);
        assert_eq!(index_substr(&s("a"), &s("")), 0);
    }
}
