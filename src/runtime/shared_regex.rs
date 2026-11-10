//! The second-level cache of compiled dynamic regexes, shared by the workers of a parallel run
//! (`-p`). Each worker keeps its own `RegexCache` as the first level, which needs no lock; on a
//! miss there, a worker looks here before compiling, so that a pattern is compiled once per run
//! rather than once per worker, and its compiled regex (`Arc<AwkRegex>`) is shared.
//!
//! The entries are bounded by a process-level memory budget (`regex_memory::BUDGET`), which also
//! counts the leftmost-longest machinery built for them (see `AwkRegex::counted_shared`). Each
//! worker searches with a copy of a shared regex, with search caches of its own (see
//! `AwkRegex::share`): those caches, and the matchers for patterns not compiled yet, stay in the
//! budget of the worker's thread, which is not divided among the workers. They are what the
//! worker's searches need: with an eighth of the budget each, a worker cycling through a table of
//! patterns evicts and rebuilds them on every record, several times slower, and the memory freed
//! is taken up again by the churn.

use super::awk_regex::AwkRegex;
use super::regex_memory;
use hashbrown::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const SHARDS: usize = 16;

type Entries = HashMap<Box<[u8]>, (Arc<AwkRegex>, u64, usize), foldhash::fast::FixedState>;

#[derive(Default)]
struct Shard {
    /// The regexes compiled without and with IGNORECASE.
    maps: [Entries; 2],
    /// Counts the uses of the entries, for evicting the least recently used ones. Per shard, so
    /// that workers looking up different patterns do not contend on one counter.
    clock: u64,
}

pub(crate) struct SharedRegexes {
    shards: Box<[Mutex<Shard>]>,
    /// The memory of the entries and of the leftmost-longest machinery built for them.
    used: Arc<AtomicUsize>,
    budget: usize,
}

impl SharedRegexes {
    pub(crate) fn new() -> SharedRegexes {
        SharedRegexes::with_budget(regex_memory::BUDGET)
    }

    pub(crate) fn with_budget(budget: usize) -> SharedRegexes {
        SharedRegexes {
            shards: (0..SHARDS).map(|_| Mutex::default()).collect(),
            used: Arc::default(),
            budget,
        }
    }

    /// The counter the leftmost-longest machinery of the regexes stored here is charged to.
    pub(crate) fn used_counter(&self) -> Arc<AtomicUsize> {
        self.used.clone()
    }

    #[cfg(test)]
    pub(crate) fn used(&self) -> usize {
        self.used.load(Ordering::Relaxed)
    }

    fn shard(&self, pat: &[u8]) -> &Mutex<Shard> {
        use std::hash::BuildHasher;
        let h = foldhash::fast::FixedState::default().hash_one(pat);
        &self.shards[(h >> 32) as usize % SHARDS]
    }

    /// The regex compiled for `pat` (with IGNORECASE as `ignore_case`) and its memory, if a
    /// worker compiled it.
    pub(crate) fn get(&self, ignore_case: bool, pat: &[u8]) -> Option<(Arc<AwkRegex>, usize)> {
        let mut shard = self.shard(pat).lock().unwrap_or_else(|e| e.into_inner());
        shard.clock += 1;
        let tick = shard.clock;
        let (re, t, cost) = shard.maps[ignore_case as usize].get_mut(pat)?;
        *t = tick;
        Some((re.clone(), *cost))
    }

    /// Stores `re`, compiled for `pat`, which took `cost` bytes. Returns the regex stored for
    /// `pat`: another worker may have compiled it in the meantime, and that one is kept.
    pub(crate) fn insert(
        &self,
        ignore_case: bool,
        pat: &[u8],
        re: &Arc<AwkRegex>,
        cost: usize,
    ) -> Arc<AwkRegex> {
        let cost =
            cost + pat.len() + std::mem::size_of::<(Box<[u8]>, (Arc<AwkRegex>, u64, usize))>();
        let mut shard = self.shard(pat).lock().unwrap_or_else(|e| e.into_inner());
        shard.clock += 1;
        let tick = shard.clock;
        if let Some((existing, t, _)) = shard.maps[ignore_case as usize].get_mut(pat) {
            *t = tick;
            return existing.clone();
        }
        if self.used.load(Ordering::Relaxed) + cost > self.budget {
            self.evict_lru(&mut shard);
        }
        self.used.fetch_add(cost, Ordering::Relaxed);
        shard.maps[ignore_case as usize].insert(pat.into(), (re.clone(), tick, cost));
        re.clone()
    }

    /// Evicts the least recently used half of the entries of `shard`: the shards fill evenly, so
    /// the inserts that find the cache full evict from all of them in turn.
    fn evict_lru(&self, shard: &mut Shard) {
        let mut ticks: Vec<u64> = shard
            .maps
            .iter()
            .flat_map(|m| m.values().map(|(_, t, _)| *t))
            .collect();
        if ticks.is_empty() {
            return;
        }
        let mid = ticks.len() / 2;
        let (_, &mut threshold, _) = ticks.select_nth_unstable(mid);
        let mut freed = 0;
        for m in shard.maps.iter_mut() {
            m.retain(|_, (_, t, cost)| {
                let keep = *t > threshold;
                if !keep {
                    freed += *cost;
                }
                keep
            });
        }
        let _ = self
            .used
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(freed))
            });
    }
}
