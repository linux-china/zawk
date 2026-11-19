//! The column store of a DataFrame: typed columns (`int64`, `double`, `bool`, `string`) with an
//! optional null mask, and the [`Value`] of one cell.
//!
//! Values follow awk's rules: a number converts to a string with `CONVFMT` (`OFMT` when printed),
//! a string to a number as awk does (`"3x"` is 3), and a string from the input (a field, a CSV
//! cell, an argument of `df_append`) is a *strnum*: it compares numerically when it looks like a
//! number, as `$1 < 10` does.
use std::borrow::Cow;
use std::cmp::Ordering;

use crate::runtime::compare::looks_numeric;
use crate::runtime::{Float, Int, numfmt, str_escape, str_to_num};

/// The value of one cell.
#[derive(Clone, Debug)]
pub(crate) enum Value {
    Null,
    Int(Int),
    Float(Float),
    Bool(bool),
    /// The bytes of a string, and whether it is a strnum (it comes from the input).
    Str(Vec<u8>, bool),
}

impl Value {
    /// A string that is not a strnum (a constant or the result of a string function).
    pub(crate) fn str(bs: impl Into<Vec<u8>>) -> Value {
        Value::Str(bs.into(), false)
    }

    /// A string from the input: a strnum.
    pub(crate) fn input(bs: impl Into<Vec<u8>>) -> Value {
        Value::Str(bs.into(), true)
    }

    /// A number: an integer when it is a whole number that fits, as awk prints it.
    pub(crate) fn num(f: Float) -> Value {
        if f.fract() == 0.0 && f.abs() < 9.0e15 {
            Value::Int(f as Int)
        } else {
            Value::Float(f)
        }
    }

    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Whether the value is missing: null, the empty string or the `--null` text. Aggregates skip
    /// missing values, as `_sum`/`_mean` skip the empty elements of an array.
    pub(crate) fn is_missing(&self) -> bool {
        match self {
            Value::Null => true,
            Value::Str(bs, _) => bs.is_empty() || str_escape::is_null(bs),
            _ => false,
        }
    }

    /// The number awk converts this value to (`"3x"` is 3, `""` is 0).
    pub(crate) fn to_num(&self) -> Float {
        match self {
            Value::Null => 0.0,
            Value::Int(i) => *i as Float,
            Value::Float(f) => *f,
            Value::Bool(b) => *b as Int as Float,
            Value::Str(bs, _) => str_to_num(bs),
        }
    }

    /// The number this value is in a comparison, or `None` if it compares as a string: numbers,
    /// and strnums that look like numbers.
    pub(crate) fn cmp_num(&self) -> Option<Float> {
        match self {
            Value::Int(i) => Some(*i as Float),
            Value::Float(f) => Some(*f),
            Value::Bool(b) => Some(*b as Int as Float),
            Value::Str(bs, true) => looks_numeric(bs),
            Value::Str(_, false) | Value::Null => None,
        }
    }

    /// The string awk converts this value to: a float with `CONVFMT`, or with `OFMT` when
    /// `output` (for printing); a null is the `--null` text (empty by default).
    pub(crate) fn text(&self, output: bool) -> Cow<'_, [u8]> {
        match self {
            Value::Null => Cow::Borrowed(str_escape::null_input().as_bytes()),
            Value::Int(i) => Cow::Owned(itoa::Buffer::new().format(*i).as_bytes().to_vec()),
            Value::Float(f) => Cow::Owned(numfmt::with_number_bytes(*f, output, |bs| bs.to_vec())),
            Value::Bool(true) => Cow::Borrowed(&b"1"[..]),
            Value::Bool(false) => Cow::Borrowed(&b"0"[..]),
            Value::Str(bs, _) => Cow::Borrowed(&bs[..]),
        }
    }

    /// The text of the value without loss: a float in its shortest round-trip form
    /// (`0.30000000000000004`, `1234567.89`) rather than with `CONVFMT` (`1.23457e+06`), as the
    /// values of databases and Parquet files are read (`numfmt::float_to_text`). Used where a
    /// value is stored or passed on as data: keys, awk arrays, files, string columns.
    pub(crate) fn exact_text(&self) -> Cow<'_, [u8]> {
        match self {
            Value::Float(f) => Cow::Owned(numfmt::float_to_text(*f).into_bytes()),
            v => v.text(false),
        }
    }

    /// [`Value::text`] as UTF-8 (invalid sequences replaced), for the text formats.
    pub(crate) fn text_lossy(&self, output: bool) -> String {
        String::from_utf8_lossy(&self.text(output)).into_owned()
    }

    /// Whether the value is true as an awk condition: a non-zero number, a non-empty string (a
    /// strnum that looks like a number is true when it is non-zero).
    pub(crate) fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Bool(b) => *b,
            Value::Str(bs, strnum) => !crate::runtime::compare::str_is_false(bs, *strnum),
        }
    }
}

/// Compares two values with awk's rules: numerically when both are numbers (or strnums that look
/// like numbers), otherwise as strings, byte by byte. A null is the uninitialized value: 0 next to
/// a number and "" next to a string.
pub(crate) fn awk_cmp(l: &Value, r: &Value) -> Ordering {
    match (l, r) {
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, other) => match other.cmp_num() {
            Some(n) => cmp_float(0.0, n),
            None => (&[] as &[u8]).cmp(&*other.text(false)),
        },
        (other, Value::Null) => awk_cmp(r, other).reverse(),
        _ => match (l.cmp_num(), r.cmp_num()) {
            (Some(x), Some(y)) => cmp_float(x, y),
            _ => l.text(false).cmp(&r.text(false)),
        },
    }
}

pub(crate) fn cmp_float(x: Float, y: Float) -> Ordering {
    x.partial_cmp(&y).unwrap_or_else(|| x.total_cmp(&y))
}

/// The kind of data a column stores.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Int,
    Float,
    Bool,
    Str,
}

/// The kind of data of a declared type (`age:int`, the `:TYPE` suffixes of `-o parquet`).
pub(crate) fn decl_kind(decl: &str) -> Kind {
    match decl.trim().to_ascii_lowercase().as_str() {
        "int" | "int8" | "int16" | "int32" | "int64" | "integer" | "bigint" | "smallint"
        | "tinyint" | "long" => Kind::Int,
        "float" | "float32" | "float64" | "double" | "real" | "number" | "num" => Kind::Float,
        "bool" | "boolean" => Kind::Bool,
        _ => Kind::Str,
    }
}

/// The type name of a column kind, as `df_schema` and the `:TYPE` suffix of `df_write` show it.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Int => "int64",
        Kind::Float => "double",
        Kind::Bool => "bool",
        Kind::Str => "string",
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ColData {
    Int(Vec<Int>),
    Float(Vec<Float>),
    Bool(Vec<bool>),
    Str(Vec<Box<[u8]>>),
}

impl ColData {
    fn empty(kind: Kind) -> ColData {
        match kind {
            Kind::Int => ColData::Int(Vec::new()),
            Kind::Float => ColData::Float(Vec::new()),
            Kind::Bool => ColData::Bool(Vec::new()),
            Kind::Str => ColData::Str(Vec::new()),
        }
    }

    fn kind(&self) -> Kind {
        match self {
            ColData::Int(_) => Kind::Int,
            ColData::Float(_) => Kind::Float,
            ColData::Bool(_) => Kind::Bool,
            ColData::Str(_) => Kind::Str,
        }
    }

    fn len(&self) -> usize {
        match self {
            ColData::Int(v) => v.len(),
            ColData::Float(v) => v.len(),
            ColData::Bool(v) => v.len(),
            ColData::Str(v) => v.len(),
        }
    }
}

/// A column: a name, an optional declared type (`decimal(12,2)`, `date`, ... kept for the
/// `:TYPE` suffix of the output), the data, and which cells are null.
#[derive(Clone, Debug)]
pub(crate) struct Column {
    pub(crate) name: String,
    pub(crate) decl: Option<String>,
    pub(crate) data: ColData,
    /// `nulls[i]` is set for a null cell; `None` when no cell is null.
    pub(crate) nulls: Option<Vec<bool>>,
    /// For a string column: whether its values are strnums (from the input), which compare
    /// numerically when they look like numbers. A column of string constants or results of
    /// string functions (`df_mutate(d, "code = '007'")`) is not.
    pub(crate) strnum: bool,
}

impl Column {
    /// An empty column; its kind is that of the declared type, a string column by default.
    pub(crate) fn new(name: impl Into<String>, decl: Option<String>) -> Column {
        let kind = decl.as_deref().map_or(Kind::Str, decl_kind);
        Column {
            name: name.into(),
            decl,
            data: ColData::empty(kind),
            nulls: None,
            strnum: true,
        }
    }

    /// A column holding `values`: of the declared type, or else of the narrowest kind that holds
    /// every value (int, then double, then bool, then string).
    pub(crate) fn from_values(
        name: impl Into<String>,
        decl: Option<String>,
        values: Vec<Value>,
    ) -> Column {
        let kind = match decl.as_deref() {
            Some(d) => decl_kind(d),
            None => infer_kind(&values),
        };
        // strnums unless every value is a plain string (numbers become strnums as text)
        let strnum = !values.iter().any(|v| matches!(v, Value::Str(_, false)))
            || values.iter().any(|v| !matches!(v, Value::Str(_, false) | Value::Null));
        let mut col = Column {
            name: name.into(),
            decl,
            data: ColData::empty(kind),
            nulls: None,
            strnum,
        };
        for v in values {
            col.push(v);
        }
        col
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn kind(&self) -> Kind {
        self.data.kind()
    }

    pub(crate) fn is_null(&self, i: usize) -> bool {
        self.nulls.as_ref().is_some_and(|n| n[i])
    }

    /// The type of the column: the declared one, or the name of its kind.
    pub(crate) fn type_name(&self) -> String {
        match &self.decl {
            Some(d) => d.clone(),
            None => kind_name(self.kind()).to_string(),
        }
    }

    /// The header of the column in the TSV stream the `-o parquet/avro/jsonl/xlsx` writers read:
    /// `name:TYPE` for a declared or numeric column, the plain name for a string column.
    pub(crate) fn typed_header(&self) -> String {
        match (&self.decl, self.kind()) {
            (Some(d), _) => format!("{}:{}", self.name, d),
            (None, Kind::Str) => self.name.clone(),
            (None, kind) => format!("{}:{}", self.name, kind_name(kind)),
        }
    }

    pub(crate) fn get(&self, i: usize) -> Value {
        if self.is_null(i) {
            return Value::Null;
        }
        match &self.data {
            ColData::Int(v) => Value::Int(v[i]),
            ColData::Float(v) => Value::Float(v[i]),
            ColData::Bool(v) => Value::Bool(v[i]),
            ColData::Str(v) => Value::Str(v[i].to_vec(), self.strnum),
        }
    }

    /// Records whether the value just pushed to the data is null.
    fn push_null_flag(&mut self, null: bool) {
        // the cells before the one just pushed
        let before = self.len() - 1;
        match &mut self.nulls {
            Some(n) => n.push(null),
            None if null => {
                let mut n = vec![false; before];
                n.push(true);
                self.nulls = Some(n);
            }
            None => {}
        }
    }

    /// Whether `v` can be stored in this column without changing its kind.
    fn fits(&self, v: &Value) -> bool {
        if self.decl.is_some() {
            // A declared column converts every value to its type.
            return true;
        }
        match (self.kind(), v) {
            (_, Value::Null) | (Kind::Str, _) => true,
            (Kind::Int, Value::Int(_) | Value::Bool(_)) => true,
            (Kind::Float, Value::Int(_) | Value::Float(_) | Value::Bool(_)) => true,
            (Kind::Bool, Value::Bool(_)) => true,
            _ => false,
        }
    }

    /// Changes the kind of the column to `kind`, converting the values stored so far.
    fn promote(&mut self, kind: Kind) {
        let values: Vec<Value> = (0..self.len()).map(|i| self.get(i)).collect();
        self.data = ColData::empty(kind);
        self.nulls = None;
        for v in values {
            self.push_converted(v);
        }
    }

    /// Appends a value, widening the kind of an undeclared column if needed (int to double, and
    /// anything else to string), and converting it to the type of a declared one (a value that
    /// is not of that type is null).
    pub(crate) fn push(&mut self, v: Value) {
        if !self.fits(&v) {
            let to = match (self.kind(), &v) {
                (Kind::Int, Value::Float(_)) => Kind::Float,
                _ => Kind::Str,
            };
            self.promote(to);
        }
        self.push_converted(v);
    }

    fn push_converted(&mut self, v: Value) {
        if v.is_null() {
            match &mut self.data {
                ColData::Int(d) => d.push(0),
                ColData::Float(d) => d.push(0.0),
                ColData::Bool(d) => d.push(false),
                ColData::Str(d) => d.push(Box::default()),
            }
            self.push_null_flag(true);
            return;
        }
        let ok = match &mut self.data {
            ColData::Int(d) => match to_int(&v) {
                Some(i) => {
                    d.push(i);
                    true
                }
                None => {
                    d.push(0);
                    false
                }
            },
            ColData::Float(d) => match to_float(&v) {
                Some(f) => {
                    d.push(f);
                    true
                }
                None => {
                    d.push(0.0);
                    false
                }
            },
            ColData::Bool(d) => match to_bool(&v) {
                Some(b) => {
                    d.push(b);
                    true
                }
                None => {
                    d.push(false);
                    false
                }
            },
            ColData::Str(d) => {
                d.push(v.exact_text().into_owned().into_boxed_slice());
                if !matches!(v, Value::Str(_, false)) {
                    self.strnum = true;
                }
                true
            }
        };
        self.push_null_flag(!ok);
    }

    /// The rows `idx` of the column, in that order.
    pub(crate) fn take(&self, idx: &[usize]) -> Column {
        let data = match &self.data {
            ColData::Int(v) => ColData::Int(idx.iter().map(|&i| v[i]).collect()),
            ColData::Float(v) => ColData::Float(idx.iter().map(|&i| v[i]).collect()),
            ColData::Bool(v) => ColData::Bool(idx.iter().map(|&i| v[i]).collect()),
            ColData::Str(v) => ColData::Str(idx.iter().map(|&i| v[i].clone()).collect()),
        };
        let nulls = self.nulls.as_ref().and_then(|n| {
            let taken: Vec<bool> = idx.iter().map(|&i| n[i]).collect();
            taken.iter().any(|b| *b).then_some(taken)
        });
        Column {
            name: self.name.clone(),
            decl: self.decl.clone(),
            data,
            nulls,
            strnum: self.strnum,
        }
    }

    /// The rows `idx` of the column, a null for `None` (the missing side of an outer join).
    pub(crate) fn take_opt(&self, idx: &[Option<usize>]) -> Column {
        let mut col = Column {
            name: self.name.clone(),
            decl: self.decl.clone(),
            data: ColData::empty(self.kind()),
            nulls: None,
            strnum: self.strnum,
        };
        for i in idx {
            col.push_converted(match i {
                Some(i) => self.get(*i),
                None => Value::Null,
            });
        }
        col
    }

    /// The same column under another name.
    pub(crate) fn renamed(&self, name: &str) -> Column {
        let mut col = self.clone();
        col.name = name.to_string();
        col
    }
}

/// The narrowest kind holding every non-null value: int, then double, then bool, then string.
/// A strnum is a string: CSV text stays text unless types are inferred (see `infer_text_kind`).
fn infer_kind(values: &[Value]) -> Kind {
    let (mut int, mut num, mut boolean, mut any) = (true, true, true, false);
    for v in values {
        match v {
            Value::Null => continue,
            Value::Int(_) => boolean = false,
            Value::Float(_) => (int, boolean) = (false, false),
            Value::Bool(_) => (int, num) = (false, false),
            Value::Str(..) => return Kind::Str,
        }
        any = true;
    }
    match () {
        _ if !any => Kind::Str,
        _ if int => Kind::Int,
        _ if num => Kind::Float,
        _ if boolean => Kind::Bool,
        _ => Kind::Str,
    }
}

/// The kind of a text column inferred from its values (`infer_types=1`): int if every non-empty
/// value is an integer, double if every one is a number, bool if every one is `true`/`false`,
/// and string otherwise.
pub(crate) fn infer_text_kind(values: &[Value]) -> Kind {
    let (mut int, mut num, mut boolean, mut any) = (true, true, true, false);
    for v in values {
        let Value::Str(bs, _) = v else {
            continue;
        };
        let text = String::from_utf8_lossy(bs);
        let text = text.trim();
        if text.is_empty() || str_escape::is_null(bs) {
            continue;
        }
        any = true;
        if crate::runtime::typed_value::parse_int(text).is_none() {
            int = false;
        }
        if crate::runtime::typed_value::parse_float(text).is_none() {
            num = false;
        }
        if !matches!(text.to_ascii_lowercase().as_str(), "true" | "false") {
            boolean = false;
        }
        if !(int || num || boolean) {
            return Kind::Str;
        }
    }
    match () {
        _ if !any => Kind::Str,
        _ if int => Kind::Int,
        _ if num => Kind::Float,
        _ if boolean => Kind::Bool,
        _ => Kind::Str,
    }
}

/// A column of inferred kind `kind` from text values: empty cells are null.
pub(crate) fn typed_text_column(name: String, kind: Kind, values: Vec<Value>) -> Column {
    if kind == Kind::Str {
        return Column::from_values(name, None, values);
    }
    let mut col = Column {
        name,
        decl: None,
        data: ColData::empty(kind),
        nulls: None,
        strnum: true,
    };
    for v in values {
        let v = if v.is_missing() { Value::Null } else { v };
        col.push_converted(v);
    }
    col
}

fn to_int(v: &Value) -> Option<Int> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(*i),
        Value::Float(f) => f.is_finite().then_some(*f as Int),
        Value::Bool(b) => Some(*b as Int),
        Value::Str(bs, _) => {
            let text = String::from_utf8_lossy(bs);
            let text = text.trim();
            if text.is_empty() {
                return None;
            }
            crate::runtime::typed_value::parse_int(text)
                .or_else(|| looks_numeric(text.as_bytes()).filter(|f| f.is_finite()).map(|f| f as Int))
        }
    }
}

fn to_float(v: &Value) -> Option<Float> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(*i as Float),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(*b as Int as Float),
        Value::Str(bs, _) => looks_numeric(bs),
    }
}

fn to_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Null => None,
        Value::Int(i) => Some(*i != 0),
        Value::Float(f) => Some(*f != 0.0),
        Value::Bool(b) => Some(*b),
        Value::Str(bs, _) => {
            let text = String::from_utf8_lossy(bs);
            match text.trim().to_ascii_lowercase().as_str() {
                "true" | "t" | "yes" | "y" | "1" => Some(true),
                "false" | "f" | "no" | "n" | "0" => Some(false),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widening() {
        let mut c = Column::from_values("x", None, vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(c.kind(), Kind::Int);
        c.push(Value::Float(2.5));
        assert_eq!(c.kind(), Kind::Float);
        c.push(Value::str("a"));
        assert_eq!(c.kind(), Kind::Str);
        assert_eq!(&*c.get(2).text(false), b"2.5");
        assert_eq!(&*c.get(3).text(false), b"a");
    }

    #[test]
    fn declared() {
        let mut c = Column::new("age", Some("int".into()));
        c.push(Value::input("42"));
        c.push(Value::input(""));
        c.push(Value::input("x"));
        assert_eq!(c.kind(), Kind::Int);
        assert!(matches!(c.get(0), Value::Int(42)));
        assert!(c.get(1).is_null());
        assert!(c.get(2).is_null());
        assert_eq!(c.typed_header(), "age:int");
    }

    #[test]
    fn string_columns() {
        // constants stay strings, input text is strnum
        let c = Column::from_values("code", None, vec![Value::str("007"), Value::Null]);
        assert!(matches!(c.get(0), Value::Str(_, false)));
        let c = Column::from_values("code", None, vec![Value::input("007")]);
        assert!(matches!(c.get(0), Value::Str(_, true)));
        // a float stored as text keeps its digits
        let mut c = Column::from_values("x", None, vec![Value::str("a")]);
        c.push(Value::Float(1234567.89));
        assert_eq!(&*c.get(1).text(false), b"1234567.89");
    }

    #[test]
    fn strnum_comparisons() {
        // "10" from the input compares numerically with 9, a constant "10" as a string.
        assert_eq!(awk_cmp(&Value::input("10"), &Value::Int(9)), Ordering::Greater);
        assert_eq!(awk_cmp(&Value::str("10"), &Value::input("9")), Ordering::Less);
        // "" < 1 as in awk
        assert_eq!(awk_cmp(&Value::input(""), &Value::Int(1)), Ordering::Less);
        assert_eq!(awk_cmp(&Value::Null, &Value::Int(0)), Ordering::Equal);
    }
}
