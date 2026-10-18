//! The memory budget shared by the caches of regexes built at runtime: the dynamic regexes of
//! `RegexCache` (compiled ones, with their leftmost-longest machinery, and `LightMatcher`s,
//! for both IGNORECASE settings) and what `awk_regex` builds from regexes for record separators
//! (`EXTENDERS`). Dynamic regexes can come from input data (`$0 ~ $1`), so these
//! caches are bounded by memory, measured with the allocator's counters for the thread while an
//! entry is built: the size of a compiled regex depends on its character classes much more than
//! on the length of its pattern (`[[:alpha:]]x1` takes ~110KB, as `[[:alpha:]]` is
//! `\p{Alphabetic}`, `x1` ~6KB).
//!
//! When adding an entry would take the caches of the thread past the budget, an eviction round
//! starts (see `make_room`): every cache evicts its least recently used half, the caches of
//! `awk_regex` right away and those of `RegexCache` the next time it is used.

use std::cell::Cell;

/// The memory budget, in bytes, of the regex caches of a thread.
const BUDGET: usize = 64 << 20;

thread_local! {
    static LIMIT: Cell<usize> = const { Cell::new(BUDGET) };
    // The measured memory of the entries of the caches.
    static USED: Cell<usize> = const { Cell::new(0) };
    // The number of eviction rounds so far.
    static ROUND: Cell<u64> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn used() -> usize {
    USED.get()
}

/// The current eviction round: a cache that took part in an earlier one evicts its least
/// recently used half before it is used again.
pub(crate) fn round() -> u64 {
    ROUND.get()
}

pub(crate) fn charge(cost: usize) {
    USED.set(USED.get() + cost);
}

pub(crate) fn release(cost: usize) {
    USED.set(USED.get().saturating_sub(cost));
}

#[cfg(test)]
pub(crate) fn set_budget(budget: usize) {
    LIMIT.set(budget);
}

/// Called before adding an entry of `cost` bytes to a cache: when that would take the caches past
/// the budget, starts a new eviction round and returns true; the caller then evicts its least
/// recently used half before adding the entry.
pub(crate) fn make_room(cost: usize) -> bool {
    if USED.get() + cost <= LIMIT.get() {
        return false;
    }
    ROUND.set(ROUND.get() + 1);
    super::awk_regex::evict_caches();
    true
}

/// Runs `f`, also returning the number of bytes it left allocated on this thread, or `None` when
/// the allocator does not tell; callers then estimate the memory instead.
pub(crate) fn measure<T>(f: impl FnOnce() -> T) -> (T, Option<usize>) {
    let before = live_bytes();
    let t = f();
    let after = live_bytes();
    let used = before.zip(after).map(|(b, a)| a.saturating_sub(b) as usize);
    (t, used)
}

/// The number of bytes allocated, and not freed yet, by this thread (counting from when the
/// counters started).
#[cfg(target_family = "unix")]
fn live_bytes() -> Option<u64> {
    use tikv_jemalloc_ctl::thread::{allocatedp, deallocatedp, ThreadLocal};
    thread_local! {
        static COUNTERS: Option<(ThreadLocal<u64>, ThreadLocal<u64>)> = counters();
    }
    fn counters() -> Option<(ThreadLocal<u64>, ThreadLocal<u64>)> {
        let (allocated, deallocated) = (allocatedp::read().ok()?, deallocatedp::read().ok()?);
        // The counters only move when jemalloc is the global allocator.
        let before = allocated.get();
        drop(std::hint::black_box(vec![0u8; 4096]));
        (allocated.get() != before).then_some((allocated, deallocated))
    }
    COUNTERS.with(|c| c.map(|(a, d)| a.get().wrapping_sub(d.get())))
}

#[cfg(not(target_family = "unix"))]
fn live_bytes() -> Option<u64> {
    None
}
