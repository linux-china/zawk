//! `df_group(d, "keys", "aggs")`: grouping and aggregates.
//!
//! The aggregates are `sum mean (avg) min max median quantile(x, q) var stddev mode count
//! count_distinct first last concat(x[, sep])`; their argument is an expression over the columns
//! (`sum(price * qty)`). Missing values (null, `""`, the `--null` text) are skipped, as the
//! `_sum`/`_mean`/... array functions skip them, and the statistics are those of
//! `runtime::array_stats`: `median(x)` of a group is `_median(a)` of an array holding the same
//! values. `count()` counts the rows, `count(x)` the values that are not missing.
use std::collections::{HashMap, HashSet};

use super::DataFrame;
use super::column::{Column, Value, awk_cmp};
use super::expr::{EvalCtx, Expr, Parser, Res, Tok};
use super::ops::{RowKey, parse_names, row_key};
use crate::runtime::Float;
use crate::runtime::array_stats::{
    STAT_MAX, STAT_MEAN, STAT_MIN, STAT_MODE, STAT_QUANTILE, STAT_STDDEV, STAT_SUM, STAT_VAR,
    stat_of,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AggKind {
    Sum,
    Mean,
    Min,
    Max,
    Median,
    Quantile,
    Var,
    Stddev,
    Mode,
    Count,
    CountDistinct,
    First,
    Last,
    Concat,
}

fn agg_kind(name: &str) -> Option<AggKind> {
    Some(match name.to_ascii_lowercase().as_str() {
        "sum" => AggKind::Sum,
        "mean" | "avg" => AggKind::Mean,
        "min" => AggKind::Min,
        "max" => AggKind::Max,
        "median" => AggKind::Median,
        "quantile" => AggKind::Quantile,
        "var" => AggKind::Var,
        "stddev" | "std" => AggKind::Stddev,
        "mode" => AggKind::Mode,
        "count" => AggKind::Count,
        "count_distinct" => AggKind::CountDistinct,
        "first" => AggKind::First,
        "last" => AggKind::Last,
        "concat" => AggKind::Concat,
        _ => return None,
    })
}

/// One aggregate of an aggregate list: `sum(amount) as total`.
pub(super) struct Agg {
    pub(super) kind: AggKind,
    /// The argument, bound to the table; `None` for `count()`.
    arg: Option<Expr>,
    /// The `q` of `quantile(x, q)`.
    q: Float,
    /// The separator of `concat(x, sep)`.
    sep: Vec<u8>,
    pub(super) name: String,
}

/// Parses and binds the aggregates of `spec` against `df`.
pub(super) fn parse_aggs(df: &DataFrame, spec: &str) -> Res<Vec<Agg>> {
    let mut p = Parser::new(spec)?;
    let mut aggs = Vec::new();
    let no_params = HashMap::new();
    while !p.at_end() {
        let e = p.parse_expr(0)?;
        let alias = if p.eat_word("as") {
            match p.next() {
                Some(Tok::Ident(n)) | Some(Tok::ColName(n)) => Some(n),
                Some(Tok::Str(s)) => Some(String::from_utf8_lossy(&s).into_owned()),
                _ => return Err(p.error("expected a name after `as`")),
            }
        } else {
            None
        };
        if !p.eat(&Tok::Comma) && !p.at_end() {
            return Err(p.error("expected `,`"));
        }
        let Expr::Call(fname, mut args) = e else {
            return Err(format!(
                "expected an aggregate such as `sum(x)` or `count()` in `{}`",
                spec
            ));
        };
        let Some(kind) = agg_kind(&fname) else {
            return Err(format!(
                "unknown aggregate `{}` (expected sum, mean, min, max, median, quantile, var, stddev, mode, count, count_distinct, first, last or concat)",
                fname
            ));
        };
        let (min, max) = match kind {
            AggKind::Count => (0, 1),
            AggKind::Quantile => (2, 2),
            AggKind::Concat => (1, 2),
            _ => (1, 1),
        };
        if args.len() < min || args.len() > max {
            return Err(format!(
                "aggregate `{}` expects {} argument{}, got {}",
                fname,
                if min == max {
                    min.to_string()
                } else {
                    format!("{} or {}", min, max)
                },
                if max == 1 { "" } else { "s" },
                args.len()
            ));
        }
        let hint = args.first().and_then(Expr::column_hint);
        let name = alias.unwrap_or_else(|| match &hint {
            Some(h) => format!("{}_{}", fname.to_ascii_lowercase(), h),
            None => fname.to_ascii_lowercase(),
        });
        // The constant argument (`q`, `sep`).
        let mut extra = None;
        if args.len() > 1 {
            let mut e = args.pop().unwrap();
            e.bind(df, &no_params)?;
            match e {
                Expr::Lit(v) => extra = Some(v),
                _ => {
                    return Err(format!(
                        "the second argument of `{}` must be a constant",
                        fname
                    ));
                }
            }
        }
        let q = match (kind, &extra) {
            (AggKind::Quantile, Some(v)) => v.to_num(),
            (AggKind::Median, _) => 0.5,
            _ => 0.0,
        };
        let sep = match (kind, &extra) {
            (AggKind::Concat, Some(v)) => v.text(false).into_owned(),
            _ => b",".to_vec(),
        };
        let arg = match args.pop() {
            Some(mut e) => {
                e.bind(df, &no_params)?;
                Some(e)
            }
            None => None,
        };
        aggs.push(Agg {
            kind,
            arg,
            q,
            sep,
            name,
        });
    }
    if aggs.is_empty() {
        return Err("no aggregate given".to_string());
    }
    Ok(aggs)
}

/// The rows of each distinct value of the columns `cols`, in the order of their first rows.
pub(super) fn group_rows(df: &DataFrame, cols: &[usize]) -> Vec<Vec<usize>> {
    let mut ids: HashMap<RowKey, usize> = HashMap::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in 0..df.nrows {
        let key = row_key(df, cols, i);
        let id = *ids.entry(key).or_insert_with(|| {
            groups.push(Vec::new());
            groups.len() - 1
        });
        groups[id].push(i);
    }
    groups
}

/// The value of the aggregate `agg` over the rows `rows` of `df`.
pub(super) fn apply(agg: &Agg, df: &DataFrame, rows: &[usize], ctx: &mut EvalCtx) -> Res<Value> {
    let Some(arg) = &agg.arg else {
        // count()
        return Ok(Value::Int(rows.len() as i64));
    };
    let mut values = Vec::with_capacity(rows.len());
    for &i in rows {
        values.push(arg.eval(df, i, ctx)?);
    }
    let present = || values.iter().filter(|v| !v.is_missing());
    let stat = |kind| Value::num(stat_of(present().map(Value::to_num).collect(), kind, agg.q));
    Ok(match agg.kind {
        AggKind::Sum => stat(STAT_SUM),
        AggKind::Mean => stat(STAT_MEAN),
        AggKind::Median | AggKind::Quantile => stat(STAT_QUANTILE),
        AggKind::Var => stat(STAT_VAR),
        AggKind::Stddev => stat(STAT_STDDEV),
        AggKind::Mode => stat(STAT_MODE),
        AggKind::Min | AggKind::Max => {
            // Numbers numerically (as `_min`/`_max`); otherwise as awk compares (dates, names).
            if present().all(|v| v.cmp_num().is_some()) {
                stat(if agg.kind == AggKind::Min {
                    STAT_MIN
                } else {
                    STAT_MAX
                })
            } else {
                let best = present().reduce(|a, b| {
                    let ord = awk_cmp(b, a);
                    let better = if agg.kind == AggKind::Min {
                        ord == std::cmp::Ordering::Less
                    } else {
                        ord == std::cmp::Ordering::Greater
                    };
                    if better { b } else { a }
                });
                best.cloned().unwrap_or(Value::Null)
            }
        }
        AggKind::Count => Value::Int(present().count() as i64),
        AggKind::CountDistinct => {
            let distinct: HashSet<Vec<u8>> = present().map(|v| v.text(false).into_owned()).collect();
            Value::Int(distinct.len() as i64)
        }
        AggKind::First => values.first().cloned().unwrap_or(Value::Null),
        AggKind::Last => values.last().cloned().unwrap_or(Value::Null),
        AggKind::Concat => {
            let mut out = Vec::new();
            for (i, v) in present().enumerate() {
                if i > 0 {
                    out.extend_from_slice(&agg.sep);
                }
                out.extend_from_slice(&v.text(false));
            }
            Value::str(out)
        }
    })
}

/// `df_group(d, "keys", "aggs")`: one row per distinct value of the key columns (in the order of
/// their first rows), with the key columns and the aggregates. Without keys (`""`), the
/// aggregates of the whole table.
pub(super) fn group(df: &DataFrame, keys: &str, aggs: &str) -> Res<DataFrame> {
    let keys = if keys.trim().is_empty() {
        Vec::new()
    } else {
        parse_names(keys)?
    };
    let key_cols: Vec<usize> = keys
        .iter()
        .map(|(name, _)| df.col(name))
        .collect::<Res<_>>()?;
    let aggs = parse_aggs(df, aggs)?;
    let groups = if key_cols.is_empty() {
        vec![(0..df.nrows).collect()]
    } else {
        group_rows(df, &key_cols)
    };
    // (a group of `group_rows` is never empty; the one group of a table without rows is)
    let firsts: Vec<usize> = groups.iter().map(|g| g.first().copied().unwrap_or(0)).collect();
    let mut out = DataFrame::default();
    for ((_, alias), &c) in keys.iter().zip(&key_cols) {
        let col = df.columns[c].take(&firsts);
        out.add_column(match alias {
            Some(a) => col.renamed(a),
            None => col,
        });
    }
    let mut ctx = EvalCtx::default();
    for agg in &aggs {
        let mut values = Vec::with_capacity(groups.len());
        for rows in &groups {
            values.push(apply(agg, df, rows, &mut ctx)?);
        }
        out.add_column(Column::from_values(agg.name.clone(), None, values));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::df::ops::new;

    fn orders() -> DataFrame {
        let mut df = new("city,amount").unwrap();
        for (c, a) in [
            ("Paris", "10"),
            ("Rome", "5"),
            ("Paris", "2.5"),
            ("Rome", ""),
            ("Oslo", "1"),
        ] {
            df.push_row(vec![Value::input(c), Value::input(a)]);
        }
        df
    }

    fn texts(df: &DataFrame, col: &str) -> Vec<String> {
        let c = &df.columns[df.col(col).unwrap()];
        (0..df.nrows).map(|i| c.get(i).text_lossy(true)).collect()
    }

    #[test]
    fn group_by_city() {
        let g = group(
            &orders(),
            "city",
            "sum(amount) as total, count() as n, count(amount), max(amount), concat(amount, '|')",
        )
        .unwrap();
        assert_eq!(g.names(), ["city", "total", "n", "count_amount", "max_amount", "concat_amount"]);
        assert_eq!(texts(&g, "city"), ["Paris", "Rome", "Oslo"]);
        assert_eq!(texts(&g, "total"), ["12.5", "5", "1"]);
        assert_eq!(texts(&g, "n"), ["2", "2", "1"]);
        assert_eq!(texts(&g, "count_amount"), ["2", "1", "1"]);
        assert_eq!(texts(&g, "max_amount"), ["10", "5", "1"]);
        assert_eq!(texts(&g, "concat_amount"), ["10|2.5", "5", "1"]);
    }

    #[test]
    fn whole_table() {
        let g = group(&orders(), "", "median(amount), quantile(amount, 1), min(city)").unwrap();
        assert_eq!(g.nrows, 1);
        assert_eq!(texts(&g, "median_amount"), ["3.75"]);
        assert_eq!(texts(&g, "quantile_amount"), ["10"]);
        assert_eq!(texts(&g, "min_city"), ["Oslo"]);
        assert!(group(&orders(), "city", "nope(amount)").is_err());
    }
}
