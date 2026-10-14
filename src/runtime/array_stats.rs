//! Exact statistics over the values of an array: `_median`, `_quantile`, `_var`, `_stddev`,
//! `_mode`, `_count_distinct` and `_top`.
//!
//! Unlike `_sum`/`_mean`, these accept any array: string values (e.g. `a[NR] = $3`) are converted
//! to numbers as awk does, and the keys may be strings (e.g. `c[$1]++; _top(c, 10)`).

use std::cmp::Ordering;
use std::hash::Hash;

use hashbrown::HashSet;

use crate::runtime::sorted_in::SortItem;
use crate::runtime::{Float, Int, IntMap, SharedMap, Str};

/// The statistic computed by `array_stat`: `_median(a)` and `_quantile(a, q)` are `STAT_QUANTILE`.
pub(crate) const STAT_QUANTILE: Int = 0;
pub(crate) const STAT_VAR: Int = 1;
pub(crate) const STAT_STDDEV: Int = 2;
pub(crate) const STAT_MODE: Int = 3;

/// A value of an array as seen by `_count_distinct`: numbers compare by value, strings by bytes.
pub(crate) trait DistinctItem {
    fn distinct_key(&self) -> DistinctKey;
}

#[derive(PartialEq, Eq, Hash)]
pub(crate) enum DistinctKey {
    Int(Int),
    Float(u64),
    Str(Vec<u8>),
}

impl DistinctItem for Int {
    fn distinct_key(&self) -> DistinctKey {
        DistinctKey::Int(*self)
    }
}

impl DistinctItem for Float {
    fn distinct_key(&self) -> DistinctKey {
        // An integral float is the same value as the integer, and -0 is 0.
        if self.fract() == 0.0 && self.abs() < 9.0e15 {
            DistinctKey::Int(*self as Int)
        } else if self.is_nan() {
            DistinctKey::Float(Float::NAN.to_bits())
        } else {
            DistinctKey::Float(self.to_bits())
        }
    }
}

impl<'a> DistinctItem for Str<'a> {
    fn distinct_key(&self) -> DistinctKey {
        DistinctKey::Str(self.with_bytes(|bs| bs.to_vec()))
    }
}

fn cmp_num(x: Float, y: Float) -> Ordering {
    x.partial_cmp(&y).unwrap_or_else(|| x.total_cmp(&y))
}

/// The values of `map` as numbers.
fn numbers<K, V>(map: &SharedMap<K, V>) -> Vec<Float>
where
    K: Hash + Eq,
    V: SortItem,
{
    map.iter(|it| it.map(|(_, v)| v.sort_num()).collect())
}

/// The `q`-quantile (`0 <= q <= 1`, clamped) of `xs`, interpolating linearly between the two
/// closest ranks (as R's default type 7 and numpy's `quantile`). 0 for an empty array.
fn quantile(mut xs: Vec<Float>, q: Float) -> Float {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_unstable_by(|a, b| cmp_num(*a, *b));
    let q = if q.is_nan() { 0.5 } else { q.clamp(0.0, 1.0) };
    let pos = q * (xs.len() - 1) as Float;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        xs[lo]
    } else {
        xs[lo] + (xs[hi] - xs[lo]) * (pos - lo as Float)
    }
}

/// The sample variance (divided by `n - 1`, as SQL's `VAR_SAMP` and R's `var`) of `xs`; 0 for
/// fewer than two values. Uses Welford's algorithm for numerical stability.
fn variance(xs: &[Float]) -> Float {
    if xs.len() < 2 {
        return 0.0;
    }
    let (mut mean, mut m2) = (0.0, 0.0);
    for (i, x) in xs.iter().enumerate() {
        let delta = x - mean;
        mean += delta / (i + 1) as Float;
        m2 += delta * (x - mean);
    }
    m2 / (xs.len() - 1) as Float
}

/// The most frequent value of `xs`, the smallest one on a tie; 0 for an empty array.
fn mode(mut xs: Vec<Float>) -> Float {
    xs.sort_unstable_by(|a, b| cmp_num(*a, *b));
    let (mut best, mut best_count) = (0.0, 0);
    let mut i = 0;
    while i < xs.len() {
        let mut j = i + 1;
        while j < xs.len() && xs[j] == xs[i] {
            j += 1;
        }
        if j - i > best_count {
            best = xs[i];
            best_count = j - i;
        }
        i = j;
    }
    best
}

/// `_median(a)`, `_quantile(a, q)`, `_var(a)`, `_stddev(a)` and `_mode(a)`: the statistic `kind`
/// (one of the `STAT_*` constants) of the values of `map`.
pub(crate) fn array_stat<K, V>(map: &SharedMap<K, V>, kind: Int, q: Float) -> Float
where
    K: Hash + Eq,
    V: SortItem,
{
    let xs = numbers(map);
    match kind {
        STAT_QUANTILE => quantile(xs, q),
        STAT_VAR => variance(&xs),
        STAT_STDDEV => variance(&xs).sqrt(),
        STAT_MODE => mode(xs),
        _ => 0.0,
    }
}

/// `_count_distinct(a)`: the number of distinct values of `map`.
pub(crate) fn count_distinct<K, V>(map: &SharedMap<K, V>) -> Int
where
    K: Hash + Eq,
    V: DistinctItem,
{
    map.iter(|it| it.map(|(_, v)| v.distinct_key()).collect::<HashSet<_>>().len() as Int)
}

/// `_top(a, k)`: the (at most) `k` keys of `map` with the largest (numeric) values, from the
/// largest down, as an array indexed from 1. Keys with equal values are ordered by their string.
pub(crate) fn top<'a, K, V>(map: &SharedMap<K, V>, k: Int) -> IntMap<Str<'a>>
where
    K: Hash + Eq + Clone + SortItem + Into<Str<'a>>,
    V: SortItem,
{
    let res = IntMap::default();
    if k <= 0 {
        return res;
    }
    let mut items: Vec<(K, Float, Vec<u8>)> = map.iter(|it| {
        it.map(|(key, v)| (key.clone(), v.sort_num(), key.sort_str().into_owned())).collect()
    });
    let cmp = |a: &(K, Float, Vec<u8>), b: &(K, Float, Vec<u8>)| {
        cmp_num(b.1, a.1).then_with(|| a.2.cmp(&b.2))
    };
    let k = (k as usize).min(items.len());
    if k < items.len() {
        items.select_nth_unstable_by(k, cmp);
        items.truncate(k);
    }
    items.sort_unstable_by(cmp);
    for (i, (key, _, _)) in items.into_iter().enumerate() {
        res.insert(i as Int + 1, key.into());
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::StrMap;

    fn floats(xs: &[Float]) -> IntMap<Float> {
        let m = IntMap::default();
        for (i, x) in xs.iter().enumerate() {
            m.insert(i as Int + 1, *x);
        }
        m
    }

    #[test]
    fn quantiles() {
        let m = floats(&[3.0, 1.0, 4.0, 2.0]);
        assert_eq!(array_stat(&m, STAT_QUANTILE, 0.5), 2.5);
        assert_eq!(array_stat(&m, STAT_QUANTILE, 0.0), 1.0);
        assert_eq!(array_stat(&m, STAT_QUANTILE, 1.0), 4.0);
        assert_eq!(array_stat(&m, STAT_QUANTILE, 0.25), 1.75);
        assert_eq!(array_stat(&floats(&[]), STAT_QUANTILE, 0.5), 0.0);
    }

    #[test]
    fn variance_stddev_mode() {
        let m = floats(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert!((array_stat(&m, STAT_VAR, 0.0) - 32.0 / 7.0).abs() < 1e-12);
        assert!((array_stat(&m, STAT_STDDEV, 0.0) - (32.0f64 / 7.0).sqrt()).abs() < 1e-12);
        assert_eq!(array_stat(&m, STAT_MODE, 0.0), 4.0);
        assert_eq!(array_stat(&floats(&[3.0, 1.0, 3.0, 1.0]), STAT_MODE, 0.0), 1.0);
    }

    #[test]
    fn distinct_and_top() {
        let m: StrMap<Int> = StrMap::default();
        for (k, v) in [("a", 3), ("b", 5), ("c", 3), ("d", 1)] {
            m.insert(Str::from(k), v);
        }
        assert_eq!(count_distinct(&m), 3);
        let t = top(&m, 3);
        assert_eq!(t.len(), 3);
        assert_eq!(t.get(&1), Str::from("b"));
        assert_eq!(t.get(&2), Str::from("a"));
        assert_eq!(t.get(&3), Str::from("c"));
        assert_eq!(top(&m, 0).len(), 0);
        assert_eq!(top(&m, 10).len(), 4);
    }
}
