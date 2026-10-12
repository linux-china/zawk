//! gawk's `PROCINFO["sorted_in"]`: the order in which `for (k in arr)` visits the elements.
//!
//! Only the predefined orders (`@ind_str_asc`, `@val_num_desc`, ...) are supported; a
//! user-defined comparison function is a runtime error rather than being silently ignored.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::hash::Hash;

use crate::common::Result;
use crate::runtime::compare::looks_numeric;
use crate::runtime::float_parse::strtod;
use crate::runtime::{Float, Int, SharedMap, Str, StrMap};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Key {
    IndStr,
    IndNum,
    ValType,
    ValStr,
    ValNum,
}

/// A parsed `PROCINFO["sorted_in"]` value. `None` is the default (unsorted) order.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct SortedIn(Option<(Key, bool /* descending */)>);

impl SortedIn {
    pub(crate) const UNSORTED: SortedIn = SortedIn(None);

    pub(crate) fn parse(s: &[u8]) -> Result<SortedIn> {
        let key = match s {
            b"" | b"@unsorted" => return Ok(SortedIn::UNSORTED),
            b"@ind_str_asc" | b"@ind_str_desc" => Key::IndStr,
            b"@ind_num_asc" | b"@ind_num_desc" => Key::IndNum,
            b"@val_type_asc" | b"@val_type_desc" => Key::ValType,
            b"@val_str_asc" | b"@val_str_desc" => Key::ValStr,
            b"@val_num_asc" | b"@val_num_desc" => Key::ValNum,
            _ if s.first() == Some(&b'@') => {
                return err!(
                    "PROCINFO[\"sorted_in\"]: `{}' is not a valid sort order",
                    String::from_utf8_lossy(s)
                )
            }
            _ => {
                return err!(
                    "PROCINFO[\"sorted_in\"]: user-defined comparison functions (`{}') are not supported, use one of the predefined orders such as \"@ind_str_asc\"",
                    String::from_utf8_lossy(s)
                )
            }
        };
        Ok(SortedIn(Some((key, s.ends_with(b"_desc")))))
    }

    /// Reads the order from the PROCINFO array.
    pub(crate) fn from_procinfo(procinfo: &StrMap<Str>) -> Result<SortedIn> {
        // A single lookup, which (unlike `get`) does not insert the key.
        let key = Str::from("sorted_in");
        match procinfo.0.borrow().get(&key) {
            Some(v) => v.with_bytes(SortedIn::parse),
            None => Ok(SortedIn::UNSORTED),
        }
    }
}

/// A key or value of an array, as seen by the sort orders.
pub(crate) trait SortItem: Clone {
    /// The string value.
    fn sort_str(&self) -> Cow<'_, [u8]>;
    /// The numeric value.
    fn sort_num(&self) -> Float;
    /// The numeric value if this is a number for `@val_type_*`; strings that look numeric
    /// count as numbers (as with `asort`), since strings are untyped at runtime.
    fn sort_type_num(&self) -> Option<Float>;
}

impl SortItem for Int {
    fn sort_str(&self) -> Cow<'_, [u8]> {
        Cow::Owned(self.to_string().into_bytes())
    }
    fn sort_num(&self) -> Float {
        *self as Float
    }
    fn sort_type_num(&self) -> Option<Float> {
        Some(*self as Float)
    }
}

impl SortItem for Float {
    fn sort_str(&self) -> Cow<'_, [u8]> {
        Cow::Owned(Str::from(*self).with_bytes(|bs| bs.to_vec()))
    }
    fn sort_num(&self) -> Float {
        *self
    }
    fn sort_type_num(&self) -> Option<Float> {
        Some(*self)
    }
}

impl<'a> SortItem for Str<'a> {
    fn sort_str(&self) -> Cow<'_, [u8]> {
        Cow::Owned(self.with_bytes(|bs| bs.to_vec()))
    }
    fn sort_num(&self) -> Float {
        self.with_bytes(strtod)
    }
    fn sort_type_num(&self) -> Option<Float> {
        self.with_bytes(looks_numeric)
    }
}

fn cmp_num(x: Float, y: Float) -> Ordering {
    x.partial_cmp(&y).unwrap_or_else(|| x.total_cmp(&y))
}

/// The keys of `map`, in the given order. Ties are broken by the index string (as in gawk) so the
/// order is deterministic.
pub(crate) fn sorted_keys<K, V>(map: &SharedMap<K, V>, order: SortedIn) -> Vec<K>
where
    K: Hash + Eq + SortItem,
    V: SortItem,
{
    let Some((key, desc)) = order.0 else {
        return map.to_vec();
    };
    let mut res: Vec<K> = match key {
        Key::IndStr => sort_by_cached(map.to_vec(), |k| k.sort_str().into_owned(), |a, b| a.cmp(b)),
        Key::IndNum => sort_by_cached(
            map.to_vec(),
            |k| (k.sort_num(), k.sort_str().into_owned()),
            |a, b| cmp_num(a.0, b.0).then_with(|| a.1.cmp(&b.1)),
        ),
        Key::ValStr | Key::ValNum | Key::ValType => {
            let pairs: Vec<(K, V)> = map.iter(|it| it.map(|(k, v)| (k.clone(), v.clone())).collect());
            let mut items: Vec<(K, ValKey, Vec<u8>)> = pairs
                .into_iter()
                .map(|(k, v)| {
                    let vk = match key {
                        Key::ValStr => ValKey::Str(v.sort_str().into_owned()),
                        Key::ValNum => ValKey::Num(v.sort_num(), v.sort_str().into_owned()),
                        _ => match v.sort_type_num() {
                            Some(n) => ValKey::Num(n, Vec::new()),
                            None => ValKey::Str(v.sort_str().into_owned()),
                        },
                    };
                    let ks = k.sort_str().into_owned();
                    (k, vk, ks)
                })
                .collect();
            items.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.2.cmp(&b.2)));
            items.into_iter().map(|(k, _, _)| k).collect()
        }
    };
    if desc {
        res.reverse();
    }
    res
}

/// A value's sort key. Numbers come before strings (for `@val_type_*`); numbers with the same
/// value are ordered by their string value (for `@val_num_*`).
enum ValKey {
    Num(Float, Vec<u8>),
    Str(Vec<u8>),
}

impl ValKey {
    fn cmp(&self, other: &ValKey) -> Ordering {
        match (self, other) {
            (ValKey::Num(x, xs), ValKey::Num(y, ys)) => cmp_num(*x, *y).then_with(|| xs.cmp(ys)),
            (ValKey::Num(..), ValKey::Str(_)) => Ordering::Less,
            (ValKey::Str(_), ValKey::Num(..)) => Ordering::Greater,
            (ValKey::Str(x), ValKey::Str(y)) => x.cmp(y),
        }
    }
}

fn sort_by_cached<K, C>(keys: Vec<K>, f: impl Fn(&K) -> C, cmp: impl Fn(&C, &C) -> Ordering) -> Vec<K> {
    let mut items: Vec<(C, K)> = keys.into_iter().map(|k| (f(&k), k)).collect();
    items.sort_by(|a, b| cmp(&a.0, &b.0));
    items.into_iter().map(|(_, k)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::IntMap;

    fn order(s: &str) -> SortedIn {
        SortedIn::parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn parse_orders() {
        assert_eq!(order(""), SortedIn::UNSORTED);
        assert_eq!(order("@unsorted"), SortedIn::UNSORTED);
        assert_eq!(order("@ind_num_desc"), SortedIn(Some((Key::IndNum, true))));
        assert!(SortedIn::parse(b"@bogus").is_err());
        assert!(SortedIn::parse(b"my_cmp").is_err());
    }

    #[test]
    fn int_keys() {
        let m: IntMap<Int> = IntMap::default();
        for (k, v) in [(10, 1), (9, 3), (100, 2)] {
            m.insert(k, v);
        }
        assert_eq!(sorted_keys(&m, order("@ind_str_asc")), vec![10, 100, 9]);
        assert_eq!(sorted_keys(&m, order("@ind_num_asc")), vec![9, 10, 100]);
        assert_eq!(sorted_keys(&m, order("@ind_num_desc")), vec![100, 10, 9]);
        assert_eq!(sorted_keys(&m, order("@val_num_asc")), vec![10, 100, 9]);
        assert_eq!(sorted_keys(&m, order("@val_num_desc")), vec![9, 100, 10]);
    }

    #[test]
    fn str_values() {
        let m: StrMap<Str> = StrMap::default();
        for (k, v) in [("a", "pear"), ("b", "10"), ("c", "9"), ("d", "apple"), ("e", "9")] {
            m.insert(k.into(), v.into());
        }
        let keys = |o| -> Vec<String> {
            sorted_keys(&m, order(o)).iter().map(|k| k.to_string()).collect()
        };
        assert_eq!(keys("@val_str_asc"), vec!["b", "c", "e", "d", "a"]);
        assert_eq!(keys("@val_type_asc"), vec!["c", "e", "b", "d", "a"]);
        assert_eq!(keys("@val_type_desc"), vec!["a", "d", "b", "e", "c"]);
    }
}
