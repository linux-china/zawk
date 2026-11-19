//! `df_join(a, b, "on"[, how])`: hash joins.
//!
//! `on` names the key columns: `id` (the same name in both tables), `user_id=id` (a column of
//! `a`, then one of `b`), or several separated by commas. `how` is `inner` (the default), `left`,
//! `right`, `full`, `semi` (the rows of `a` that have a match) or `anti` (those that do not).
//! Keys match when their strings are equal (see `ops::key_of`); a null key matches nothing, as in
//! SQL.
//!
//! The result has the columns of `a`, then those of `b` without its key columns; a column of `b`
//! whose name is taken gets the suffix `_right`. For the rows of `b` without a match (`right`,
//! `full`), the key columns of `a` hold the keys of `b`.
use std::collections::HashMap;
use std::sync::Arc;

use super::DataFrame;
use super::column::{Column, Value};
use super::expr::Res;
use super::ops::{RowKey, row_key, split_list};

#[derive(Clone, Copy, PartialEq, Eq)]
enum How {
    Inner,
    Left,
    Right,
    Full,
    Semi,
    Anti,
}

fn parse_how(how: &str) -> Res<How> {
    Ok(match how.trim().to_ascii_lowercase().as_str() {
        "" | "inner" => How::Inner,
        "left" => How::Left,
        "right" => How::Right,
        "full" | "outer" | "full_outer" => How::Full,
        "semi" => How::Semi,
        "anti" => How::Anti,
        other => {
            return Err(format!(
                "unknown join \"{}\" (expected inner, left, right, full, semi or anti)",
                other
            ));
        }
    })
}

fn has_null(key: &RowKey) -> bool {
    key.iter().any(|(null, _)| *null)
}

pub(super) fn join(left: &DataFrame, right: &DataFrame, on: &str, how: &str) -> Res<DataFrame> {
    let how = parse_how(how)?;
    let mut lkeys = Vec::new();
    let mut rkeys = Vec::new();
    for item in split_list(on) {
        let (l, r) = match item.split_once("==").or_else(|| item.split_once('=')) {
            Some((l, r)) => (l.trim().to_string(), r.trim().to_string()),
            None => (item.clone(), item.clone()),
        };
        lkeys.push(left.col(&l).map_err(|e| format!("left table: {}", e))?);
        rkeys.push(right.col(&r).map_err(|e| format!("right table: {}", e))?);
    }
    if lkeys.is_empty() {
        return Err("no join columns given".to_string());
    }
    let mut index: HashMap<RowKey, Vec<usize>> = HashMap::new();
    for r in 0..right.nrows {
        let key = row_key(right, &rkeys, r);
        if !has_null(&key) {
            index.entry(key).or_default().push(r);
        }
    }
    let mut pairs: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let mut matched = vec![false; right.nrows];
    for l in 0..left.nrows {
        let key = row_key(left, &lkeys, l);
        let hits = if has_null(&key) {
            None
        } else {
            index.get(&key)
        };
        match (how, hits) {
            (How::Semi, Some(_)) => pairs.push((Some(l), None)),
            (How::Anti, None) => pairs.push((Some(l), None)),
            (How::Semi | How::Anti, _) => {}
            (_, Some(rows)) => {
                for &r in rows {
                    matched[r] = true;
                    pairs.push((Some(l), Some(r)));
                }
            }
            (How::Left | How::Full, None) => pairs.push((Some(l), None)),
            (How::Inner | How::Right, None) => {}
        }
    }
    if matches!(how, How::Right | How::Full) {
        for (r, m) in matched.iter().enumerate() {
            if !m {
                pairs.push((None, Some(r)));
            }
        }
    }
    let lidx: Vec<Option<usize>> = pairs.iter().map(|p| p.0).collect();
    let mut columns: Vec<Column> = Vec::new();
    for (c, col) in left.columns.iter().enumerate() {
        match lkeys.iter().position(|&k| k == c) {
            // A key column: the key of `b` for the rows that only `b` has.
            Some(k) if matches!(how, How::Right | How::Full) => {
                let rcol = &right.columns[rkeys[k]];
                let values: Vec<Value> = pairs
                    .iter()
                    .map(|(l, r)| match (l, r) {
                        (Some(l), _) => col.get(*l),
                        (None, Some(r)) => rcol.get(*r),
                        (None, None) => Value::Null,
                    })
                    .collect();
                columns.push(Column::from_values(col.name.clone(), col.decl.clone(), values));
            }
            _ => columns.push(col.take_opt(&lidx)),
        }
    }
    if !matches!(how, How::Semi | How::Anti) {
        let ridx: Vec<Option<usize>> = pairs.iter().map(|p| p.1).collect();
        for (c, col) in right.columns.iter().enumerate() {
            if rkeys.contains(&c) {
                continue;
            }
            let mut name = col.name.clone();
            while columns.iter().any(|x| x.name == name) {
                name.push_str("_right");
            }
            let mut taken = col.take_opt(&ridx);
            taken.name = name;
            columns.push(taken);
        }
    }
    let nrows = pairs.len();
    Ok(DataFrame {
        columns: columns.into_iter().map(Arc::new).collect(),
        nrows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::df::ops::new;

    fn table(schema: &str, rows: &[&[&str]]) -> DataFrame {
        let mut df = new(schema).unwrap();
        for r in rows {
            df.push_row(r.iter().map(|v| Value::input(*v)).collect());
        }
        df
    }

    fn texts(df: &DataFrame, col: &str) -> Vec<String> {
        let c = &df.columns[df.col(col).unwrap()];
        (0..df.nrows).map(|i| c.get(i).text_lossy(true)).collect()
    }

    #[test]
    fn joins() {
        let users = table("id,name", &[&["1", "Ann"], &["2", "Bob"], &["3", "Cy"]]);
        let orders = table(
            "oid,user_id,amount",
            &[&["a", "1", "10"], &["b", "1", "5"], &["c", "4", "7"]],
        );
        let inner = join(&orders, &users, "user_id=id", "").unwrap();
        assert_eq!(inner.names(), ["oid", "user_id", "amount", "name"]);
        assert_eq!(texts(&inner, "name"), ["Ann", "Ann"]);
        let left = join(&orders, &users, "user_id=id", "left").unwrap();
        assert_eq!(texts(&left, "name"), ["Ann", "Ann", ""]);
        let full = join(&orders, &users, "user_id = id", "full").unwrap();
        assert_eq!(texts(&full, "user_id"), ["1", "1", "4", "2", "3"]);
        let semi = join(&users, &orders, "id=user_id", "semi").unwrap();
        assert_eq!(texts(&semi, "name"), ["Ann"]);
        let anti = join(&users, &orders, "id=user_id", "anti").unwrap();
        assert_eq!(texts(&anti, "name"), ["Bob", "Cy"]);
        let same = join(&users, &users, "id", "inner").unwrap();
        assert_eq!(same.names(), ["id", "name", "name_right"]);
        assert!(join(&users, &orders, "nope", "").is_err());
        assert!(join(&users, &orders, "id=user_id", "sideways").is_err());
    }
}
