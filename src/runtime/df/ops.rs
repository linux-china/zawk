//! Row and column operations: `df_new`, `df_select`, `df_drop`, `df_filter`, `df_mutate`,
//! `df_sort`, `df_head`/`df_tail`/`df_slice`, `df_distinct` and `df_concat`.
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::DataFrame;
use super::column::{Column, Value, cmp_float};
use super::expr::{self, EvalCtx, Parser, Res, Tok};

/// Splits `s` at the commas that are not inside parentheses or quotes
/// (`"a, amount:decimal(12,2)"` is two items).
pub(super) fn split_list(s: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut quote) = (0i32, None);
    for c in s.chars() {
        match (c, quote) {
            ('"' | '\'', None) => quote = Some(c),
            (q, Some(open)) if q == open => quote = None,
            ('(', None) => depth += 1,
            (')', None) => depth -= 1,
            (',', None) if depth == 0 => {
                items.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    items.push(cur);
    items
        .into_iter()
        .map(|i| i.trim().to_string())
        .filter(|i| !i.is_empty())
        .collect()
}

/// A name token of a list: an identifier, `$name`, `$"name"`, `"name"` or `$3`.
fn name_token(tok: Option<Tok>) -> Option<String> {
    match tok? {
        Tok::Ident(n) | Tok::ColName(n) => Some(n),
        Tok::Str(s) => Some(String::from_utf8_lossy(&s).into_owned()),
        Tok::ColNum(n) => Some(n.to_string()),
        Tok::Num(n, true) => Some((n as i64).to_string()),
        _ => None,
    }
}

/// A list of column names, each with an optional alias: `"a, b as x, \"first name\""`. Names
/// that are not identifiers (`user-id`) may also be written as they are.
pub(super) fn parse_names(spec: &str) -> Res<Vec<(String, Option<String>)>> {
    let parsed = (|| -> Res<Vec<(String, Option<String>)>> {
        let mut p = Parser::new(spec)?;
        let mut out = Vec::new();
        while !p.at_end() {
            let name = name_token(p.next()).ok_or_else(|| p.error("expected a column name"))?;
            let alias = if p.eat_word("as") {
                Some(name_token(p.next()).ok_or_else(|| p.error("expected a name after `as`"))?)
            } else {
                None
            };
            out.push((name, alias));
            if !p.eat(&Tok::Comma) && !p.at_end() {
                return Err(p.error("expected `,`"));
            }
        }
        Ok(out)
    })();
    match parsed {
        Ok(names) => Ok(names),
        // plain names such as `user-id` or `size (kb)`
        Err(_) => Ok(split_list(spec)
            .into_iter()
            .map(|item| match item.split_once(" as ") {
                Some((n, a)) => (unquote(n), Some(unquote(a))),
                None => (unquote(&item), None),
            })
            .collect()),
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim().trim_start_matches('$');
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

/// The indexes of the columns of `spec` (names or numbers).
pub(super) fn column_indexes(df: &DataFrame, spec: &str) -> Res<Vec<usize>> {
    parse_names(spec)?
        .iter()
        .map(|(name, _)| df.col(name))
        .collect()
}

/// `df_new("name,age:int,city")`: an empty table with these columns (`:TYPE` declares a type,
/// as for `-o parquet`).
pub(super) fn new(schema: &str) -> Res<DataFrame> {
    let mut df = DataFrame::default();
    for item in split_list(schema) {
        let (name, decl) = crate::inspect::split_type(&item);
        let name = unquote(&name);
        if df.columns.iter().any(|c| c.name == name) {
            return Err(format!("duplicate column \"{}\"", name));
        }
        df.add_column(Column::new(name, decl.map(|d| d.to_ascii_lowercase())));
    }
    Ok(df)
}

/// `df_select(d, "a, b as x")`, or with `drop` `df_drop(d, "a, b")`.
pub(super) fn select(df: &DataFrame, spec: &str, drop: bool) -> Res<DataFrame> {
    let names = parse_names(spec)?;
    let mut columns = Vec::new();
    if drop {
        let mut dropped = HashSet::new();
        for (name, _) in &names {
            dropped.insert(df.col(name)?);
        }
        for (i, c) in df.columns.iter().enumerate() {
            if !dropped.contains(&i) {
                columns.push(c.clone());
            }
        }
    } else {
        for (name, alias) in &names {
            let c = &df.columns[df.col(name)?];
            let c = match alias {
                Some(a) => Arc::new(c.renamed(a)),
                None => c.clone(),
            };
            if columns.iter().any(|x: &Arc<Column>| x.name == c.name) {
                return Err(format!(
                    "duplicate column \"{}\" (rename it with `as`)",
                    c.name
                ));
            }
            columns.push(c);
        }
    }
    Ok(DataFrame {
        nrows: if columns.is_empty() { 0 } else { df.nrows },
        columns,
    })
}

/// `df_filter(d, expr)`: the rows for which `expr` is true.
pub(super) fn filter(
    df: &DataFrame,
    src: &str,
    params: &HashMap<String, Value>,
) -> Res<DataFrame> {
    let mut e = expr::parse_cached(src)?;
    e.bind(df, params)?;
    let mut ctx = EvalCtx::default();
    let mut keep = Vec::new();
    for i in 0..df.nrows {
        if e.eval(df, i, &mut ctx)?.truthy() {
            keep.push(i);
        }
    }
    Ok(df.take(&keep))
}

/// `df_mutate(d, "x = a * 2, y = upper(name)")`: adds (or replaces) columns, in order, so that
/// an expression may use the columns assigned before it.
pub(super) fn mutate(
    df: &DataFrame,
    spec: &str,
    params: &HashMap<String, Value>,
) -> Res<DataFrame> {
    let mut p = Parser::new(spec)?;
    let mut assignments = Vec::new();
    while !p.at_end() {
        let tok = p.next();
        // `$2 = ...` and `2 = ...` name the 2nd column (as `$2` does on the right side), not a
        // new column called "2"
        let pos = match tok {
            Some(Tok::ColNum(n)) => Some(n as i64),
            Some(Tok::Num(n, true)) => Some(n as i64),
            _ => None,
        };
        let name = name_token(tok).ok_or_else(|| p.error("expected a column name"))?;
        p.expect(Tok::Assign, "`=`")?;
        let e = p.parse_expr(0)?;
        assignments.push((name, pos, e));
        if !p.eat(&Tok::Comma) && !p.at_end() {
            return Err(p.error("expected `,`"));
        }
    }
    let mut out = df.clone();
    let mut ctx = EvalCtx::default();
    for (name, pos, mut e) in assignments {
        // a column number is resolved against the table as it is now (so it may name a column
        // added by an earlier assignment), and the replacement keeps that column's name
        let name = match pos {
            Some(n) if n >= 1 && n as usize <= out.columns.len() => {
                out.columns[n as usize - 1].name.clone()
            }
            Some(n) => {
                return Err(format!(
                    "column ${} out of range (the table has {} columns)",
                    n,
                    out.columns.len()
                ));
            }
            None => name,
        };
        e.bind(&out, params)?;
        let mut values = Vec::with_capacity(out.nrows);
        for i in 0..out.nrows {
            values.push(e.eval(&out, i, &mut ctx)?);
        }
        out.add_column(Column::from_values(name, None, values));
    }
    Ok(out)
}

/// How a sort key compares its values.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SortMode {
    /// As awk compares: numbers (and strnums that look like numbers) numerically, other strings
    /// by their bytes.
    Auto,
    Num,
    Str,
}

/// The value of a sort key: nulls first, then numbers, then strings. Unlike `awk_cmp`, which is
/// not transitive over a mix of numbers and strings (`"10" < "2a" < "9" < "10"`), this is a total
/// order, as `sort_by` requires (it may panic otherwise).
#[derive(PartialEq)]
pub(super) enum SortVal {
    Null,
    Num(f64),
    Str(Vec<u8>),
}

pub(super) fn cmp_sort(a: &SortVal, b: &SortVal) -> Ordering {
    match (a, b) {
        (SortVal::Null, SortVal::Null) => Ordering::Equal,
        (SortVal::Null, _) => Ordering::Less,
        (_, SortVal::Null) => Ordering::Greater,
        (SortVal::Num(x), SortVal::Num(y)) => cmp_float(*x, *y),
        (SortVal::Num(_), SortVal::Str(_)) => Ordering::Less,
        (SortVal::Str(_), SortVal::Num(_)) => Ordering::Greater,
        (SortVal::Str(x), SortVal::Str(y)) => x.cmp(y),
    }
}

fn sort_val(v: &Value, mode: SortMode) -> SortVal {
    if v.is_null() {
        return SortVal::Null;
    }
    match mode {
        SortMode::Num if v.is_missing() => SortVal::Null,
        SortMode::Num => SortVal::Num(v.to_num()),
        // exact: `1.0000001` and `1.0000002` differ, although CONVFMT prints both as `1`
        SortMode::Str => SortVal::Str(v.exact_text().into_owned()),
        SortMode::Auto => match v.cmp_num() {
            Some(n) => SortVal::Num(n),
            None => SortVal::Str(v.text(false).into_owned()),
        },
    }
}

/// The sort key of `v` in the default order of `df_sort`: nulls, then numbers (and strnums that
/// look like numbers) numerically, then the other strings by their bytes.
pub(super) fn sort_key(v: &Value) -> SortVal {
    sort_val(v, SortMode::Auto)
}

/// Sorts `items` by the default order of `df_sort` of the value `key` gives (a stable sort).
pub(super) fn sort_by_value<T>(items: Vec<T>, key: impl Fn(&T) -> &Value) -> Vec<T> {
    let mut keyed: Vec<(SortVal, T)> = items.into_iter().map(|t| (sort_key(key(&t)), t)).collect();
    keyed.sort_by(|a, b| cmp_sort(&a.0, &b.0));
    keyed.into_iter().map(|(_, t)| t).collect()
}

/// `df_sort(d, "age desc, name")`: a stable sort on the keys, each `asc` (the default) or
/// `desc`, compared as awk compares (`num` or `str` force a numeric or a string comparison, as
/// do the `PROCINFO["sorted_in"]` names: `age @val_num_desc`). Nulls sort first, as the empty
/// string does.
pub(super) fn sort(df: &DataFrame, spec: &str) -> Res<DataFrame> {
    let mut p = Parser::new(spec)?;
    let mut keys: Vec<(usize, bool, SortMode)> = Vec::new();
    while !p.at_end() {
        let name = name_token(p.next()).ok_or_else(|| p.error("expected a column name"))?;
        let col = df.col(&name)?;
        let (mut desc, mut mode) = (false, SortMode::Auto);
        loop {
            match p.peek() {
                Some(Tok::Ident(w)) => {
                    match w.to_ascii_lowercase().as_str() {
                        "asc" => desc = false,
                        "desc" => desc = true,
                        "num" | "numeric" => mode = SortMode::Num,
                        "str" | "string" => mode = SortMode::Str,
                        _ => return Err(p.error("expected asc, desc, num or str")),
                    }
                    p.next();
                }
                Some(Tok::Param(order)) => {
                    // @val_num_desc, @ind_str_asc, ...
                    let order = order.to_ascii_lowercase();
                    let parts: Vec<&str> = order.split('_').collect();
                    match parts.as_slice() {
                        [_, ty, dir] if matches!(*ty, "num" | "str") && matches!(*dir, "asc" | "desc") => {
                            mode = if *ty == "num" { SortMode::Num } else { SortMode::Str };
                            desc = *dir == "desc";
                        }
                        _ => return Err(p.error("expected an order such as @val_num_desc")),
                    }
                    p.next();
                }
                _ => break,
            }
        }
        keys.push((col, desc, mode));
        if !p.eat(&Tok::Comma) && !p.at_end() {
            return Err(p.error("expected `,`"));
        }
    }
    let vals: Vec<Vec<SortVal>> = keys
        .iter()
        .map(|&(c, _, mode)| {
            let col = &df.columns[c];
            (0..df.nrows).map(|i| sort_val(&col.get(i), mode)).collect()
        })
        .collect();
    let mut idx: Vec<usize> = (0..df.nrows).collect();
    idx.sort_by(|&a, &b| {
        for (k, &(_, desc, _)) in keys.iter().enumerate() {
            let ord = cmp_sort(&vals[k][a], &vals[k][b]);
            let ord = if desc { ord.reverse() } else { ord };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    Ok(df.take(&idx))
}

/// The rows `from..from + n` (0-based), within the table.
pub(super) fn slice(df: &DataFrame, from: usize, n: usize) -> DataFrame {
    let from = from.min(df.nrows);
    let to = from.saturating_add(n).min(df.nrows);
    df.take(&(from..to).collect::<Vec<_>>())
}

/// The key of a cell for grouping, joining and removing duplicates: values are equal when their
/// exact strings are (`1` and `1.0` are, `1.0000001` and `1.0000002` are not, although CONVFMT
/// prints both as `1`), and a null differs from the empty string.
pub(super) fn key_of(v: &Value) -> (bool, Vec<u8>) {
    (v.is_null(), v.exact_text().into_owned())
}

pub(super) type RowKey = Vec<(bool, Vec<u8>)>;

pub(super) fn row_key(df: &DataFrame, cols: &[usize], row: usize) -> RowKey {
    cols.iter().map(|&c| key_of(&df.columns[c].get(row))).collect()
}

/// `df_distinct(d[, "cols"])`: the first row of each distinct value of the columns (all of them
/// by default), in their order, as `uniq(arr, "keep")`.
pub(super) fn distinct(df: &DataFrame, spec: &str) -> Res<DataFrame> {
    let cols = if spec.trim().is_empty() {
        (0..df.columns.len()).collect()
    } else {
        column_indexes(df, spec)?
    };
    let mut seen = HashSet::new();
    let keep: Vec<usize> = (0..df.nrows)
        .filter(|&i| seen.insert(row_key(df, &cols, i)))
        .collect();
    Ok(df.take(&keep))
}

/// `df_concat(a, b, ...)`: the rows of the tables one after the other, with the columns matched
/// by name (a column missing from a table is null in its rows).
pub(super) fn concat(frames: &[Arc<DataFrame>]) -> DataFrame {
    let mut names: Vec<(String, Option<String>)> = Vec::new();
    for df in frames {
        for c in &df.columns {
            if !names.iter().any(|(n, _)| *n == c.name) {
                names.push((c.name.clone(), c.decl.clone()));
            }
        }
    }
    let columns = names
        .into_iter()
        .map(|(name, decl)| {
            let mut values = Vec::new();
            for df in frames {
                match df.columns.iter().find(|c| c.name == name) {
                    Some(c) => values.extend((0..df.nrows).map(|i| c.get(i))),
                    None => values.extend((0..df.nrows).map(|_| Value::Null)),
                }
            }
            Column::from_values(name, decl, values)
        })
        .collect();
    DataFrame::from_columns(columns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn people() -> DataFrame {
        let mut df = new("name,age:int,city").unwrap();
        for (n, a, c) in [
            ("Ann", "42", "Paris"),
            ("Bob", "9", "Rome"),
            ("Cy", "10", "Paris"),
            ("Dee", "", "Rome"),
        ] {
            df.push_row(vec![Value::input(n), Value::input(a), Value::input(c)]);
        }
        df
    }

    fn col_text(df: &DataFrame, name: &str) -> Vec<String> {
        let c = &df.columns[df.col(name).unwrap()];
        (0..df.nrows).map(|i| c.get(i).text_lossy(true)).collect()
    }

    #[test]
    fn lists() {
        assert_eq!(split_list("a, b:decimal(12,2) ,c"), ["a", "b:decimal(12,2)", "c"]);
        assert_eq!(
            parse_names("a, b as x, \"first name\"").unwrap(),
            [
                ("a".to_string(), None),
                ("b".to_string(), Some("x".to_string())),
                ("first name".to_string(), None)
            ]
        );
        assert_eq!(parse_names("user-id").unwrap(), [("user-id".to_string(), None)]);
    }

    #[test]
    fn select_filter_sort() {
        let df = people();
        let s = select(&df, "name as who, age", false).unwrap();
        assert_eq!(s.names(), ["who", "age"]);
        assert_eq!(select(&df, "age", true).unwrap().names(), ["name", "city"]);
        let adults = filter(&df, "age >= 10", &HashMap::new()).unwrap();
        assert_eq!(col_text(&adults, "name"), ["Ann", "Cy"]);
        let mut params = HashMap::new();
        params.insert("c".to_string(), Value::input("Rome"));
        let rome = filter(&df, "city == @c", &params).unwrap();
        assert_eq!(col_text(&rome, "name"), ["Bob", "Dee"]);
        let sorted = sort(&df, "city desc, age").unwrap();
        // nulls first in ascending order
        assert_eq!(col_text(&sorted, "name"), ["Dee", "Bob", "Cy", "Ann"]);
        let by_name = sort(&df, "name @val_str_desc").unwrap();
        assert_eq!(col_text(&by_name, "name"), ["Dee", "Cy", "Bob", "Ann"]);
    }

    #[test]
    fn mutate_distinct_concat() {
        let df = people();
        let m = mutate(&df, "y = age * 2, z = y + 1, name = upper(name)", &HashMap::new())
            .unwrap();
        assert_eq!(col_text(&m, "z"), ["85", "19", "21", "1"]);
        assert_eq!(col_text(&m, "name"), ["ANN", "BOB", "CY", "DEE"]);
        // `$N = ...` / `N = ...` replace the Nth column, keeping its name
        let ncols = df.columns.len();
        let names: Vec<String> = df.columns.iter().map(|c| c.name.clone()).collect();
        let by_pos = mutate(&df, "$1 = upper($1)", &HashMap::new()).unwrap();
        assert_eq!(by_pos.columns.len(), ncols);
        assert_eq!(by_pos.columns[0].name, names[0]);
        assert_eq!(col_text(&by_pos, &names[0]), ["ANN", "BOB", "CY", "DEE"]);
        let by_num = mutate(&df, "1 = lower($1)", &HashMap::new()).unwrap();
        assert_eq!(by_num.columns.len(), ncols);
        assert_eq!(col_text(&by_num, &names[0]), ["ann", "bob", "cy", "dee"]);
        // a column added by an earlier assignment can be named by its number
        let added = mutate(&df, &format!("w = 1, ${} = w + 1", ncols + 1), &HashMap::new())
            .unwrap();
        assert_eq!(added.columns.len(), ncols + 1);
        assert_eq!(col_text(&added, "w"), ["2", "2", "2", "2"]);
        assert!(mutate(&df, &format!("${} = 1", ncols + 1), &HashMap::new()).is_err());
        let d = distinct(&df, "city").unwrap();
        assert_eq!(col_text(&d, "name"), ["Ann", "Bob"]);
        let both = concat(&[Arc::new(df.clone()), Arc::new(select(&df, "name", false).unwrap())]);
        assert_eq!(both.nrows, 8);
        assert!(both.columns[1].get(7).is_null());
        assert_eq!(slice(&df, 3, 10).nrows, 1);
    }

    #[test]
    fn total_sort_order() {
        // awk_cmp is not transitive here ("10" < "2a" < "9" < "10"): nulls, numbers, strings
        let values: Vec<Value> = ["9", "2a", "", "10", "b", "-1", "2a"]
            .iter()
            .map(|v| Value::input(*v))
            .chain([Value::Null, Value::Float(1.5)])
            .collect();
        let sorted: Vec<String> = sort_by_value(values, |v| v)
            .iter()
            .map(|v| if v.is_null() { "null".into() } else { v.text_lossy(false) })
            .collect();
        assert_eq!(sorted, ["null", "-1", "1.5", "9", "10", "", "2a", "2a", "b"]);
    }
}
