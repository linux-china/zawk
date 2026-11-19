//! `df_pivot(d, "row", "col", "agg(val)")` and `df_melt(d, "id_cols"[, var[, value]])`.
use std::collections::HashMap;

use super::DataFrame;
use super::column::{Column, Value};
use super::expr::{EvalCtx, Res};
use super::group::{apply, group_rows, parse_aggs};
use super::ops::{column_indexes, key_of, parse_names, sort_by_value};

/// A wide table: one row per distinct value of the `rows` columns (in the order of their first
/// rows), one column per distinct value of the `col` column (in the order of `df_sort`: numbers
/// numerically, then the other strings), and
/// the aggregate `agg` of each combination (null when a combination has no rows).
pub(super) fn pivot(df: &DataFrame, rows: &str, col: &str, agg: &str) -> Res<DataFrame> {
    let row_cols = column_indexes(df, rows)?;
    if row_cols.is_empty() {
        return Err("no row columns given".to_string());
    }
    let col_cols = column_indexes(df, col)?;
    let &[pcol] = col_cols.as_slice() else {
        return Err(format!("expected one pivot column, got \"{}\"", col));
    };
    let mut aggs = parse_aggs(df, agg)?;
    if aggs.len() != 1 {
        return Err("expected one aggregate, such as `sum(n)`".to_string());
    }
    let agg = aggs.remove(0);
    // The distinct values of the pivot column, sorted.
    let pivot_col = &df.columns[pcol];
    let mut values: Vec<Value> = Vec::new();
    let mut seen = HashMap::new();
    for i in 0..df.nrows {
        let v = pivot_col.get(i);
        if seen.insert(key_of(&v), ()).is_none() {
            values.push(v);
        }
    }
    let values = sort_by_value(values, |v| v);
    let value_index: HashMap<(bool, Vec<u8>), usize> = values
        .iter()
        .enumerate()
        .map(|(i, v)| (key_of(v), i))
        .collect();
    let groups = group_rows(df, &row_cols);
    let firsts: Vec<usize> = groups.iter().map(|g| g[0]).collect();
    let mut out = DataFrame::default();
    for &c in &row_cols {
        out.add_column(df.columns[c].take(&firsts));
    }
    let mut ctx = EvalCtx::default();
    let mut cells: Vec<Vec<Value>> = vec![Vec::with_capacity(groups.len()); values.len()];
    for rows in &groups {
        let mut by_value: Vec<Vec<usize>> = vec![Vec::new(); values.len()];
        for &i in rows {
            by_value[value_index[&key_of(&pivot_col.get(i))]].push(i);
        }
        for (j, rows) in by_value.iter().enumerate() {
            cells[j].push(if rows.is_empty() {
                Value::Null
            } else {
                apply(&agg, df, rows, &mut ctx)?
            });
        }
    }
    for (v, cells) in values.iter().zip(cells) {
        // exact: two floats that CONVFMT prints alike are two columns
        let mut name = String::from_utf8_lossy(&v.exact_text()).into_owned();
        if name.is_empty() {
            name = "(empty)".to_string();
        }
        while out.columns.iter().any(|c| c.name == name) {
            name.push('_');
        }
        out.add_column(Column::from_values(name, None, cells));
    }
    Ok(out)
}

/// A long table: for each row and each column that is not an id column, a row with the id
/// columns, the name of the column (`variable` by default) and its value (`value`).
pub(super) fn melt(df: &DataFrame, ids: &str, var_name: &str, value_name: &str) -> Res<DataFrame> {
    let ids = parse_names(ids)?;
    let id_cols: Vec<usize> = ids.iter().map(|(n, _)| df.col(n)).collect::<Res<_>>()?;
    let var_name = if var_name.trim().is_empty() {
        "variable"
    } else {
        var_name.trim()
    };
    let value_name = if value_name.trim().is_empty() {
        "value"
    } else {
        value_name.trim()
    };
    let value_cols: Vec<usize> = (0..df.columns.len())
        .filter(|c| !id_cols.contains(c))
        .collect();
    let mut idx = Vec::with_capacity(df.nrows * value_cols.len());
    let mut vars = Vec::with_capacity(idx.capacity());
    let mut vals = Vec::with_capacity(idx.capacity());
    for i in 0..df.nrows {
        for &c in &value_cols {
            idx.push(i);
            vars.push(Value::str(df.columns[c].name.as_bytes().to_vec()));
            vals.push(df.columns[c].get(i));
        }
    }
    let mut out = DataFrame::default();
    let add = |out: &mut DataFrame, col: Column| -> Res<()> {
        // `add_column` would replace a column of the same name
        if out.columns.iter().any(|c| c.name == col.name) {
            return Err(format!(
                "duplicate column \"{}\" (choose other names for the variable and value columns)",
                col.name
            ));
        }
        out.add_column(col);
        Ok(())
    };
    for ((_, alias), &c) in ids.iter().zip(&id_cols) {
        let col = df.columns[c].take(&idx);
        add(&mut out, match alias {
            Some(a) => col.renamed(a),
            None => col,
        })?;
    }
    add(&mut out, Column::from_values(var_name, None, vars))?;
    add(&mut out, Column::from_values(value_name, None, vals))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::df::ops::new;

    fn texts(df: &DataFrame, col: &str) -> Vec<String> {
        let c = &df.columns[df.col(col).unwrap()];
        (0..df.nrows).map(|i| c.get(i).text_lossy(true)).collect()
    }

    #[test]
    fn pivot_and_melt() {
        let mut df = new("day,level,n:int").unwrap();
        for (d, l) in [("d1", "INFO"), ("d1", "WARN"), ("d2", "INFO"), ("d1", "INFO")] {
            df.push_row(vec![Value::input(d), Value::input(l), Value::Int(1)]);
        }
        let p = pivot(&df, "day", "level", "sum(n)").unwrap();
        assert_eq!(p.names(), ["day", "INFO", "WARN"]);
        assert_eq!(texts(&p, "INFO"), ["2", "1"]);
        assert_eq!(texts(&p, "WARN"), ["1", ""]);
        let m = melt(&p, "day", "", "count").unwrap();
        assert_eq!(m.names(), ["day", "variable", "count"]);
        assert_eq!(texts(&m, "variable"), ["INFO", "WARN", "INFO", "WARN"]);
        assert_eq!(texts(&m, "count"), ["2", "1", "1", ""]);
        // the names of the new columns must not take those of the id columns
        assert!(melt(&p, "day", "day", "").is_err());
        assert!(melt(&p, "day", "x", "x").is_err());
    }

    #[test]
    fn pivot_of_mixed_values() {
        // numbers and strings that awk_cmp does not order transitively ("10" < "2a" < "9" < "10")
        let mut df = new("k,v").unwrap();
        for v in ["9", "2a", "10", "b", "9"] {
            df.push_row(vec![Value::input("x"), Value::input(v)]);
        }
        let p = pivot(&df, "k", "v", "count()").unwrap();
        assert_eq!(p.names(), ["k", "9", "10", "2a", "b"]);
        assert_eq!(texts(&p, "9"), ["2"]);
    }
}
