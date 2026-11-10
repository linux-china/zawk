//! The memory budget shared by the caches of regexes built at runtime: the dynamic regexes of
//! `RegexCache` (compiled ones, with their leftmost-longest machinery, and `LightMatcher`s,
//! for both IGNORECASE settings) and what `awk_regex` builds from regexes for record separators
//! (`EXTENDERS`). Dynamic regexes can come from input data (`$0 ~ $1`), so these
//! caches are bounded by memory, measured with the allocator's counters for the thread while an
//! entry is built: the size of a compiled regex depends on its character classes much more than
//! on the length of its pattern (`[[:alpha:]]x1` takes ~110KB, as `[[:alpha:]]` is
//! `\p{Alphabetic}`, `x1` ~6KB).
//!
//! The caches order their entries by a clock they share (see `tick`), and the memory of the
//! entries is kept by the time of their last use (see `Recency`). When adding an entry would take
//! the caches of the thread past the budget, an eviction round starts (see `make_room`): the
//! least recently used half of the memory, across all the caches, is evicted, by the caches of
//! `awk_regex` right away and by those of `RegexCache` the next time it is used. A cache that
//! adds entries all the time (light matchers for patterns from the input) then evicts its own
//! old entries, rather than half of every cache: a small set of compiled regexes in use survives
//! the rounds.

use std::cell::Cell;

/// The memory budget, in bytes, of the regex caches of a thread. In a parallel run, the compiled
/// regexes the workers share have a process-level budget of the same size (see `shared_regex`).
pub(crate) const BUDGET: usize = 64 << 20;

thread_local! {
    static LIMIT: Cell<usize> = const { Cell::new(BUDGET) };
    // The measured memory of the entries of the caches.
    static USED: Cell<usize> = const { Cell::new(0) };
    // The number of eviction rounds so far.
    static ROUND: Cell<u64> = const { Cell::new(0) };
    // The entries last used before this tick are evicted in the current round.
    static CUTOFF: Cell<u64> = const { Cell::new(0) };
    static RECENCY: Recency = const { Recency::new() };
}

/// Ticks per bucket of `Recency`: the eviction rounds cut between buckets, so at this many
/// entries (one tick per use) of the half they aim at.
const TICK_SHIFT: u32 = 4;
/// The buckets `Recency` tells apart: entries not used in the last `BUCKETS << TICK_SHIFT` ticks
/// (64K uses of the caches) count as used at the same time, the oldest.
const BUCKETS: usize = 4096;

/// The clock of the caches, and the memory of their entries by the bucket (`tick >> TICK_SHIFT`)
/// of their last use, for buckets `base..base + BUCKETS` in a ring: older entries count in
/// `base`.
struct Recency {
    now: Cell<u64>,
    base: Cell<u64>,
    memory: [Cell<usize>; BUCKETS],
}

impl Recency {
    const fn new() -> Self {
        Recency {
            now: Cell::new(0),
            base: Cell::new(0),
            memory: [const { Cell::new(0) }; BUCKETS],
        }
    }
    fn slot(&self, tick: u64) -> &Cell<usize> {
        let bucket = (tick >> TICK_SHIFT).max(self.base.get());
        &self.memory[bucket as usize % BUCKETS]
    }
    fn tick(&self) -> u64 {
        let tick = self.now.get() + 1;
        self.now.set(tick);
        let base = self.base.get();
        if (tick >> TICK_SHIFT) - base == BUCKETS as u64 {
            // The new bucket takes the slot of the oldest, which joins the next one.
            let oldest = self.memory[base as usize % BUCKETS].take();
            self.base.set(base + 1);
            let next = &self.memory[(base + 1) as usize % BUCKETS];
            next.set(next.get() + oldest);
        }
        tick
    }
    fn add(&self, tick: u64, cost: usize) {
        let slot = self.slot(tick);
        slot.set(slot.get() + cost);
    }
    fn remove(&self, tick: u64, cost: usize) {
        let slot = self.slot(tick);
        slot.set(slot.get().saturating_sub(cost));
    }
    /// The tick that splits the memory of the entries in halves: the entries last used before it
    /// take about half of it (at least one bucket).
    fn median(&self) -> u64 {
        let base = self.base.get();
        let last = self.now.get() >> TICK_SHIFT;
        let total: usize = (base..=last).map(|b| self.slot(b << TICK_SHIFT).get()).sum();
        let mut before = 0;
        for b in base..=last {
            let m = self.slot(b << TICK_SHIFT).get();
            // Cut before this bucket or after it, whichever is closer to half.
            if 2 * (before + m) >= total {
                let cut = if before > 0 && total - 2 * before < 2 * (before + m) - total {
                    b
                } else {
                    b + 1
                };
                return cut << TICK_SHIFT;
            }
            before += m;
        }
        (last + 1) << TICK_SHIFT
    }
}

/// The next tick of the clock of the caches, for the use of an entry.
pub(crate) fn tick() -> u64 {
    RECENCY.with(Recency::tick)
}

/// Counts an entry of `cost` bytes used at `tick` against the budget.
pub(crate) fn charge_entry(tick: u64, cost: usize) {
    charge(cost);
    RECENCY.with(|r| r.add(tick, cost));
}

/// Releases an entry counted with `charge_entry`, last used at `tick`.
pub(crate) fn release_entry(tick: u64, cost: usize) {
    release(cost);
    RECENCY.with(|r| r.remove(tick, cost));
}

/// Notes that an entry of `cost` bytes last used at `old` is used again at `new`.
pub(crate) fn touch(old: u64, new: u64, cost: usize) {
    if old >> TICK_SHIFT != new >> TICK_SHIFT && cost > 0 {
        RECENCY.with(|r| {
            r.remove(old, cost);
            r.add(new, cost);
        });
    }
}

/// The entries last used before this tick are evicted in the current eviction round.
pub(crate) fn cutoff() -> u64 {
    CUTOFF.get()
}

#[cfg(test)]
pub(crate) fn used() -> usize {
    USED.get()
}

/// The current eviction round: a cache that took part in an earlier one evicts its entries last
/// used before `cutoff` before it is used again.
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

/// Whether an entry of `cost` bytes fits in the budget without an eviction round.
pub(crate) fn has_room(cost: usize) -> bool {
    USED.get() + cost <= LIMIT.get()
}

/// Called before adding an entry of `cost` bytes to a cache: when that would take the caches past
/// the budget, starts a new eviction round and returns true; the caller then evicts its entries
/// last used before `cutoff` before adding the entry.
pub(crate) fn make_room(cost: usize) -> bool {
    if has_room(cost) {
        return false;
    }
    CUTOFF.set(RECENCY.with(Recency::median));
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
    use tikv_jemalloc_ctl::thread::{ThreadLocal, allocatedp, deallocatedp};
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
