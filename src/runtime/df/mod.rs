//! DataFrames: in-memory tables for `END`-style work that awk arrays make awkward (grouping on
//! several keys, joins, sorting on several columns, pivots), behind plain function calls.
//!
//! A DataFrame is a *string handle* (`"df#1"`) to a table kept in a process-wide registry, so no
//! syntax or type is added to the language: a handle is stored in variables and arrays, passed to
//! and returned from functions, and printed as any string (`typeof(d)` is `"dataframe"` while the
//! table exists). Every `df_*` function that transforms a table returns a new handle; only
//! `df_append`/`df_append_record` change a table in place. `df_free` releases a table, and the
//! registry is dropped at exit.
//!
//! The registry is shared by the threads of a parallel run, so rows appended by the workers of
//! `-p` all go to the table created in BEGIN (in an unspecified order; `zawk --lint -p` reports
//! it as W307).
//!
//! Errors are those of the other stdlib functions: a warning on stderr (`ERRNO` is set, and
//! `try ... else` catches it) and an empty result (`""`, `0`).
pub(crate) mod column;
pub(crate) mod expr;
mod group;
pub(crate) mod io;
mod join;
mod ops;
mod pivot;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use column::{Column, Value};
use expr::{Res, resolve_name};

use crate::common::{CompileError, Result};
use crate::runtime::{Int, IntMap, Str, StrMap};

/// The `df_*` functions taking and returning only scalars. The functions that fill or read an
/// awk array (`df_columns`, `df_col`, `df_row`, `df_to_map`, `df_from_array`) are separate
/// builtins (see `builtins::Function`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DfOp {
    Read,
    New,
    Append,
    AppendRecord,
    Select,
    Drop,
    Filter,
    Mutate,
    Sort,
    Head,
    Tail,
    Slice,
    Distinct,
    Group,
    Join,
    Concat,
    Pivot,
    Melt,
    Nrow,
    Ncol,
    Get,
    Schema,
    Describe,
    ToCsv,
    ToJson,
    Print,
    Write,
    Free,
}

/// Any number of arguments from the least one.
const VARIADIC: usize = usize::MAX;

impl DfOp {
    pub const ALL: [DfOp; 28] = [
        DfOp::Read,
        DfOp::New,
        DfOp::Append,
        DfOp::AppendRecord,
        DfOp::Select,
        DfOp::Drop,
        DfOp::Filter,
        DfOp::Mutate,
        DfOp::Sort,
        DfOp::Head,
        DfOp::Tail,
        DfOp::Slice,
        DfOp::Distinct,
        DfOp::Group,
        DfOp::Join,
        DfOp::Concat,
        DfOp::Pivot,
        DfOp::Melt,
        DfOp::Nrow,
        DfOp::Ncol,
        DfOp::Get,
        DfOp::Schema,
        DfOp::Describe,
        DfOp::ToCsv,
        DfOp::ToJson,
        DfOp::Print,
        DfOp::Write,
        DfOp::Free,
    ];

    pub fn func_name(self) -> &'static str {
        match self {
            DfOp::Read => "df_read",
            DfOp::New => "df_new",
            DfOp::Append => "df_append",
            DfOp::AppendRecord => "df_append_record",
            DfOp::Select => "df_select",
            DfOp::Drop => "df_drop",
            DfOp::Filter => "df_filter",
            DfOp::Mutate => "df_mutate",
            DfOp::Sort => "df_sort",
            DfOp::Head => "df_head",
            DfOp::Tail => "df_tail",
            DfOp::Slice => "df_slice",
            DfOp::Distinct => "df_distinct",
            DfOp::Group => "df_group",
            DfOp::Join => "df_join",
            DfOp::Concat => "df_concat",
            DfOp::Pivot => "df_pivot",
            DfOp::Melt => "df_melt",
            DfOp::Nrow => "df_nrow",
            DfOp::Ncol => "df_ncol",
            DfOp::Get => "df_get",
            DfOp::Schema => "df_schema",
            DfOp::Describe => "df_describe",
            DfOp::ToCsv => "df_to_csv",
            DfOp::ToJson => "df_to_json",
            DfOp::Print => "df_print",
            DfOp::Write => "df_write",
            DfOp::Free => "df_free",
        }
    }

    /// The least and most numbers of arguments in a call (`VARIADIC`: no limit).
    pub fn arg_range(self) -> (usize, usize) {
        match self {
            DfOp::Read => (1, 3),
            DfOp::New => (0, 1),
            DfOp::Append => (1, VARIADIC),
            // The parser of `cfg` adds the fields of the record as a second argument.
            DfOp::AppendRecord => (1, 1),
            DfOp::Select | DfOp::Drop | DfOp::Sort => (2, 2),
            DfOp::Filter | DfOp::Mutate => (2, VARIADIC),
            DfOp::Head | DfOp::Tail | DfOp::Distinct => (1, 2),
            DfOp::Slice => (2, 3),
            DfOp::Group => (2, 3),
            DfOp::Join => (3, 4),
            DfOp::Concat => (2, VARIADIC),
            DfOp::Pivot => (4, 4),
            DfOp::Melt => (2, 4),
            DfOp::Nrow | DfOp::Ncol | DfOp::Schema | DfOp::Describe | DfOp::ToCsv
            | DfOp::ToJson | DfOp::Free => (1, 1),
            DfOp::Get => (3, 3),
            DfOp::Print => (1, 2),
            DfOp::Write => (2, 3),
        }
    }

    /// Whether the function returns a number (the others return a string: a handle or text).
    pub fn returns_int(self) -> bool {
        matches!(
            self,
            DfOp::Append
                | DfOp::AppendRecord
                | DfOp::Nrow
                | DfOp::Ncol
                | DfOp::Print
                | DfOp::Write
                | DfOp::Free
        )
    }

    /// Whether the function changes state (a table, the output): such calls are kept even when
    /// their result is unused, and are not reordered with their sibling operands.
    pub fn has_effects(self) -> bool {
        matches!(
            self,
            DfOp::Append | DfOp::AppendRecord | DfOp::Print | DfOp::Write | DfOp::Free
        )
    }

    /// The code passed to the compiled `df_call_*` intrinsics.
    pub fn code(self) -> Int {
        DfOp::ALL.iter().position(|op| *op == self).unwrap() as Int
    }

    pub fn from_code(code: Int) -> Option<DfOp> {
        usize::try_from(code)
            .ok()
            .and_then(|i| DfOp::ALL.get(i).copied())
    }
}

/// A table: columns of the same length, shared between the tables derived from each other
/// (`df_select` copies no data).
#[derive(Clone, Debug, Default)]
pub(crate) struct DataFrame {
    pub(crate) columns: Vec<Arc<Column>>,
    pub(crate) nrows: usize,
}

impl DataFrame {
    pub(crate) fn from_columns(columns: Vec<Column>) -> DataFrame {
        let nrows = columns.first().map_or(0, Column::len);
        DataFrame {
            columns: columns.into_iter().map(Arc::new).collect(),
            nrows,
        }
    }

    pub(crate) fn names(&self) -> Vec<&str> {
        self.columns.iter().map(|c| c.name.as_str()).collect()
    }

    /// The index of the column `name` (or of the 1-based column number `name`).
    pub(crate) fn col(&self, name: &str) -> Res<usize> {
        resolve_name(&self.names(), name)
    }

    /// `name`, or `name_1`, `name_2`, ... when the table already has a column `name` (as for a
    /// name repeated in a `-H` header, see `splitter::push_unique_name`).
    pub(crate) fn unique_name(&self, name: &str) -> String {
        let mut unique = name.to_string();
        let mut n = 0;
        while self.columns.iter().any(|c| c.name == unique) {
            n += 1;
            unique = format!("{}_{}", name, n);
        }
        unique
    }

    /// Adds a column, or replaces the column of the same name.
    pub(crate) fn add_column(&mut self, col: Column) {
        if self.columns.is_empty() {
            self.nrows = col.len();
        }
        match self.columns.iter().position(|c| c.name == col.name) {
            Some(i) => self.columns[i] = Arc::new(col),
            None => self.columns.push(Arc::new(col)),
        }
    }

    /// The rows `idx`, in that order.
    pub(crate) fn take(&self, idx: &[usize]) -> DataFrame {
        DataFrame {
            columns: self.columns.iter().map(|c| Arc::new(c.take(idx))).collect(),
            nrows: idx.len(),
        }
    }

    /// Appends a row: missing values are null, and extra values add columns `cN`.
    pub(crate) fn push_row(&mut self, mut values: Vec<Value>) {
        while self.columns.len() < values.len() {
            // (`cN` may be taken: `df_new("c2")`)
            let name = self.unique_name(&format!("c{}", self.columns.len() + 1));
            let mut col = Column::new(name, None);
            for _ in 0..self.nrows {
                col.push(Value::Null);
            }
            self.columns.push(Arc::new(col));
        }
        values.resize(self.columns.len(), Value::Null);
        for (col, v) in self.columns.iter_mut().zip(values) {
            Arc::make_mut(col).push(v);
        }
        self.nrows += 1;
    }
}

/// The tables, by the number of their handle.
#[derive(Default)]
struct Registry {
    next: u64,
    frames: HashMap<u64, Arc<DataFrame>>,
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(Default::default);

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    // A panic while the lock is held leaves consistent data: every change is a single insert.
    REGISTRY.lock().unwrap_or_else(|e| e.into_inner())
}

const HANDLE_PREFIX: &[u8] = b"df#";

fn handle_id(handle: &[u8]) -> Option<u64> {
    let digits = handle.strip_prefix(HANDLE_PREFIX)?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits).ok()?.parse().ok()
}

fn invalid_handle(handle: &[u8]) -> String {
    format!(
        "invalid dataframe handle \"{}\"",
        String::from_utf8_lossy(handle)
    )
}

/// The table of `handle`.
pub(crate) fn get(handle: &[u8]) -> Res<Arc<DataFrame>> {
    handle_id(handle)
        .and_then(|id| registry().frames.get(&id).cloned())
        .ok_or_else(|| invalid_handle(handle))
}

/// Registers a table and returns its handle.
pub(crate) fn insert(df: DataFrame) -> Vec<u8> {
    let mut reg = registry();
    reg.next += 1;
    let id = reg.next;
    reg.frames.insert(id, Arc::new(df));
    format!("df#{}", id).into_bytes()
}

/// Changes the table of `handle` in place (copying the columns other tables share).
fn with_mut<R>(handle: &[u8], f: impl FnOnce(&mut DataFrame) -> R) -> Res<R> {
    let id = handle_id(handle).ok_or_else(|| invalid_handle(handle))?;
    let mut reg = registry();
    let df = reg.frames.get_mut(&id).ok_or_else(|| invalid_handle(handle))?;
    Ok(f(Arc::make_mut(df)))
}

fn free(handle: &[u8]) -> bool {
    handle_id(handle).is_some_and(|id| registry().frames.remove(&id).is_some())
}

/// Whether `s` is the handle of a table: `typeof(s)` is then `"dataframe"`.
pub(crate) fn is_handle(s: &[u8]) -> bool {
    s.starts_with(HANDLE_PREFIX) && handle_id(s).is_some_and(|id| registry().frames.contains_key(&id))
}

/// The output format of the run (`-o`), which `df_print` follows by default: empty without `-o`.
static OUTPUT_FORMAT: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Sets the output format of the run (`-o csv`, `-o jsonl`, ...).
pub fn set_output_format(format: &str) {
    let format = match format {
        "ndjson" => "jsonl",
        "md" => "markdown",
        f => f,
    };
    let _ = OUTPUT_FORMAT.set(format.to_string());
}

pub(crate) fn output_format() -> &'static str {
    OUTPUT_FORMAT.get().map_or("", String::as_str)
}

/// The result of a `df_*` function.
enum Out {
    Str(Vec<u8>),
    Int(Int),
}

/// Why a `df_*` function failed: a stdlib warning (the result is empty), or an error writing the
/// output (which ends the program silently, as for `print`).
enum Fail {
    Warn(String),
    Output(CompileError),
}

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail::Warn(msg)
    }
}

/// The text of argument `i`, empty when it is not given.
fn arg(args: &[Vec<u8>], i: usize) -> &[u8] {
    args.get(i).map_or(&[][..], |a| &a[..])
}

fn arg_text(args: &[Vec<u8>], i: usize) -> String {
    String::from_utf8_lossy(arg(args, i)).into_owned()
}

/// Argument `i` as an integer, or `default` when it is not given (or empty).
fn arg_int(args: &[Vec<u8>], i: usize, default: Int) -> Int {
    match args.get(i) {
        Some(a) if !a.is_empty() => crate::runtime::str_to_num(a) as Int,
        _ => default,
    }
}

/// The parameters of `df_filter`/`df_mutate`: the arguments after the expression, as pairs of a
/// name and a value (`"min", 30`).
fn params(func: &str, args: &[Vec<u8>]) -> Res<HashMap<String, Value>> {
    if args.len() % 2 != 0 {
        return Err(format!(
            "{}: parameters are pairs of a name and a value (`\"min\", 30`), got an odd number of arguments",
            func
        ));
    }
    Ok(args
        .chunks(2)
        .map(|p| {
            let name = String::from_utf8_lossy(&p[0]);
            let name = name.trim().trim_start_matches('@').to_string();
            (name, Value::input(p[1].clone()))
        })
        .collect())
}

/// Runs the `df_*` function `op` with the arguments `args`.
fn dispatch(core: &mut crate::interp::Core, op: DfOp, args: &[Vec<u8>]) -> std::result::Result<Out, Fail> {
    let table = |i: usize| get(arg(args, i));
    let handle = |df: DataFrame| -> std::result::Result<Out, Fail> { Ok(Out::Str(insert(df))) };
    match op {
        DfOp::Read => handle(io::read(&arg_text(args, 0), &arg_text(args, 1), &arg_text(args, 2))?),
        DfOp::New => handle(ops::new(&arg_text(args, 0))?),
        DfOp::Append => {
            let values = args
                .get(1..)
                .unwrap_or(&[])
                .iter()
                .map(|a| Value::input(a.clone()))
                .collect();
            Ok(Out::Int(with_mut(arg(args, 0), |df| {
                df.push_row(values);
                df.nrows as Int
            })?))
        }
        DfOp::AppendRecord => {
            let header = fi_names(core);
            let fields: Vec<Value> = if arg(args, 1).is_empty() && args.len() > 1 {
                Vec::new()
            } else {
                arg(args, 1)
                    .split(|b| *b == b'\t')
                    // byte for byte: text that is not UTF-8 is kept as is
                    .map(|f| Value::input(io::unescape_tsv_bytes(f)))
                    .collect()
            };
            Ok(Out::Int(with_mut(arg(args, 0), |df| {
                if df.columns.is_empty() {
                    if header.is_empty() {
                        // Without `-H`, the first record is the header.
                        for (i, f) in fields.iter().enumerate() {
                            let name = f.text_lossy(false);
                            let name = if name.is_empty() {
                                format!("c{}", i + 1)
                            } else {
                                name
                            };
                            // a repeated name must add a column, not replace one: the
                            // values of the next records would go to the wrong columns
                            let name = df.unique_name(&name);
                            df.add_column(Column::new(name, None));
                        }
                        return 0;
                    }
                    for name in &header {
                        // (`FI` names are unique, but not once their `:TYPE` is cut: `a:int,a`)
                        let (name, decl) = crate::inspect::split_type(name);
                        let name = df.unique_name(&name);
                        df.add_column(Column::new(name, decl.map(|d| d.to_ascii_lowercase())));
                    }
                }
                df.push_row(fields);
                df.nrows as Int
            })?))
        }
        DfOp::Select => handle(ops::select(&*table(0)?, &arg_text(args, 1), false)?),
        DfOp::Drop => handle(ops::select(&*table(0)?, &arg_text(args, 1), true)?),
        DfOp::Filter => {
            let params = params(op.func_name(), args.get(2..).unwrap_or(&[]))?;
            handle(ops::filter(&*table(0)?, &arg_text(args, 1), &params)?)
        }
        DfOp::Mutate => {
            let params = params(op.func_name(), args.get(2..).unwrap_or(&[]))?;
            handle(ops::mutate(&*table(0)?, &arg_text(args, 1), &params)?)
        }
        DfOp::Sort => handle(ops::sort(&*table(0)?, &arg_text(args, 1))?),
        DfOp::Head => {
            let df = table(0)?;
            let n = arg_int(args, 1, 10).max(0) as usize;
            handle(ops::slice(&df, 0, n))
        }
        DfOp::Tail => {
            let df = table(0)?;
            let n = (arg_int(args, 1, 10).max(0) as usize).min(df.nrows);
            handle(ops::slice(&df, df.nrows - n, n))
        }
        DfOp::Slice => {
            let df = table(0)?;
            // 1-based, as `substr`
            let from = (arg_int(args, 1, 1).max(1) - 1) as usize;
            let n = arg_int(args, 2, Int::MAX).max(0) as usize;
            handle(ops::slice(&df, from, n))
        }
        DfOp::Distinct => handle(ops::distinct(&*table(0)?, &arg_text(args, 1))?),
        DfOp::Group => {
            let aggs = if args.len() > 2 {
                arg_text(args, 2)
            } else {
                "count()".to_string()
            };
            handle(group::group(&*table(0)?, &arg_text(args, 1), &aggs)?)
        }
        DfOp::Join => handle(join::join(
            &*table(0)?,
            &*table(1)?,
            &arg_text(args, 2),
            &arg_text(args, 3),
        )?),
        DfOp::Concat => {
            let mut frames = Vec::with_capacity(args.len());
            for i in 0..args.len() {
                frames.push(table(i)?);
            }
            handle(ops::concat(&frames))
        }
        DfOp::Pivot => handle(pivot::pivot(
            &*table(0)?,
            &arg_text(args, 1),
            &arg_text(args, 2),
            &arg_text(args, 3),
        )?),
        DfOp::Melt => handle(pivot::melt(
            &*table(0)?,
            &arg_text(args, 1),
            &arg_text(args, 2),
            &arg_text(args, 3),
        )?),
        DfOp::Nrow => Ok(Out::Int(table(0)?.nrows as Int)),
        DfOp::Ncol => Ok(Out::Int(table(0)?.columns.len() as Int)),
        DfOp::Get => {
            let df = table(0)?;
            let row = arg_int(args, 1, 0);
            let col = df.col(&arg_text(args, 2))?;
            if row < 1 || row as usize > df.nrows {
                return Err(Fail::Warn(format!(
                    "row {} out of range (the table has {} rows)",
                    row, df.nrows
                )));
            }
            Ok(Out::Str(
                df.columns[col].get(row as usize - 1).exact_text().into_owned(),
            ))
        }
        DfOp::Schema => {
            let df = table(0)?;
            let schema: Vec<String> = df
                .columns
                .iter()
                .map(|c| format!("{}:{}", c.name, c.type_name()))
                .collect();
            Ok(Out::Str(schema.join(",").into_bytes()))
        }
        DfOp::Describe => Ok(Out::Str(io::describe(&*table(0)?).into_bytes())),
        DfOp::ToCsv => Ok(Out::Str(io::render(&*table(0)?, "csv")?)),
        DfOp::ToJson => Ok(Out::Str(io::to_json(&*table(0)?).into_bytes())),
        DfOp::Print => {
            let df = table(0)?;
            let ofs = core.vars.ofs.with_bytes(|b| b.to_vec());
            let ors = core.vars.ors.with_bytes(|b| b.to_vec());
            let bytes = io::print_bytes(&df, &arg_text(args, 1), &ofs, &ors)?;
            core.write_files
                .write_printf(None, &bytes)
                .map_err(Fail::Output)?;
            Ok(Out::Int(df.nrows as Int))
        }
        DfOp::Write => {
            let df = table(0)?;
            io::write(&df, &arg_text(args, 1), &arg_text(args, 2))?;
            Ok(Out::Int(df.nrows as Int))
        }
        DfOp::Free => Ok(Out::Int(free(arg(args, 0)) as Int)),
    }
}

/// The column names of the header (`FI`), in field order.
fn fi_names(core: &crate::interp::Core) -> Vec<String> {
    let mut names: Vec<(Int, String)> = core
        .vars
        .fi
        .iter(|it| it.map(|(k, v)| (*v, k.as_str().into_owned())).collect());
    names.sort();
    names.dedup_by(|a, b| a.0 == b.0);
    names.into_iter().map(|(_, n)| n).collect()
}

fn run(core: &mut crate::interp::Core, op: DfOp, args: &[&Str]) -> Result<Option<Out>> {
    let args: Vec<Vec<u8>> = args.iter().map(|s| s.with_bytes(|b| b.to_vec())).collect();
    match dispatch(core, op, &args) {
        Ok(out) => Ok(Some(out)),
        Err(Fail::Warn(msg)) => {
            crate::runtime::stdlib_warning(op.func_name(), msg);
            Ok(None)
        }
        Err(Fail::Output(e)) => Err(e),
    }
}

/// A call of a `df_*` function returning a string (a handle or text); `Err` is an error writing
/// the output.
pub(crate) fn call_str<'a>(
    core: &mut crate::interp::Core<'a>,
    op: DfOp,
    args: &[&Str<'a>],
) -> Result<Str<'a>> {
    Ok(match run(core, op, args)? {
        Some(Out::Str(bs)) => Str::from_bytes(&bs),
        Some(Out::Int(i)) => Str::from(i),
        None => Str::default(),
    })
}

/// A call of a `df_*` function returning a number; `Err` is an error writing the output.
pub(crate) fn call_int<'a>(
    core: &mut crate::interp::Core<'a>,
    op: DfOp,
    args: &[&Str<'a>],
) -> Result<Int> {
    Ok(match run(core, op, args)? {
        Some(Out::Int(i)) => i,
        Some(Out::Str(bs)) => crate::runtime::str_to_num(&bs) as Int,
        None => 0,
    })
}

fn bytes_of(s: &Str) -> Vec<u8> {
    s.with_bytes(|b| b.to_vec())
}

fn warn<T>(func: &str, res: Res<T>, default: T) -> T {
    res.unwrap_or_else(|msg| {
        crate::runtime::stdlib_warning(func, msg);
        default
    })
}

/// `df_columns(d, arr)`: the column names in `arr[1..n]`; returns n.
pub(crate) fn columns<'a>(d: &Str<'a>, arr: &IntMap<Str<'a>>) -> Int {
    arr.clear();
    warn(
        "df_columns",
        get(&bytes_of(d)).map(|df| {
            for (i, c) in df.columns.iter().enumerate() {
                arr.insert(i as Int + 1, Str::from(c.name.clone()));
            }
            df.columns.len() as Int
        }),
        0,
    )
}

/// `df_col(d, col, arr)`: the values of a column in `arr[1..n]`; returns n.
pub(crate) fn col<'a>(d: &Str<'a>, col: &Str<'a>, arr: &IntMap<Str<'a>>) -> Int {
    arr.clear();
    warn(
        "df_col",
        get(&bytes_of(d)).and_then(|df| {
            let c = &df.columns[df.col(&col.as_str())?];
            for i in 0..df.nrows {
                arr.insert(i as Int + 1, Str::from_bytes(&c.get(i).exact_text()));
            }
            Ok(df.nrows as Int)
        }),
        0,
    )
}

/// `df_row(d, i, arr)`: row `i` (1-based) in `arr[column name]`; returns the number of columns.
pub(crate) fn row<'a>(d: &Str<'a>, i: &Str<'a>, arr: &StrMap<'a, Str<'a>>) -> Int {
    arr.clear();
    warn(
        "df_row",
        get(&bytes_of(d)).and_then(|df| {
            let i = i.with_bytes(crate::runtime::str_to_num) as Int;
            if i < 1 || i as usize > df.nrows {
                return Err(format!(
                    "row {} out of range (the table has {} rows)",
                    i, df.nrows
                ));
            }
            for c in &df.columns {
                arr.insert(
                    Str::from(c.name.clone()),
                    Str::from_bytes(&c.get(i as usize - 1).exact_text()),
                );
            }
            Ok(df.columns.len() as Int)
        }),
        0,
    )
}

/// `df_to_map(d, key, value, arr)`: `arr[key] = value` for each row (a later row wins); returns
/// the number of rows.
pub(crate) fn to_map<'a>(
    d: &Str<'a>,
    key: &Str<'a>,
    val: &Str<'a>,
    arr: &StrMap<'a, Str<'a>>,
) -> Int {
    arr.clear();
    warn(
        "df_to_map",
        get(&bytes_of(d)).and_then(|df| {
            let k = &df.columns[df.col(&key.as_str())?];
            let v = &df.columns[df.col(&val.as_str())?];
            for i in 0..df.nrows {
                arr.insert(
                    Str::from_bytes(&k.get(i).exact_text()),
                    Str::from_bytes(&v.get(i).exact_text()),
                );
            }
            Ok(df.nrows as Int)
        }),
        0,
    )
}

/// `df_from_array(arr[, "key,value"])`: a table of two columns, the keys and the values of
/// `arr`, sorted by key (numerically for numeric keys, as `@ind_num_asc`... then as strings).
pub(crate) fn from_array<'a>(arr: &StrMap<'a, Str<'a>>, names: &Str<'a>) -> Str<'a> {
    let names = names.as_str();
    let names: Vec<String> = names
        .split(',')
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect();
    let key_name = names.first().cloned().unwrap_or_else(|| "key".to_string());
    let val_name = names.get(1).cloned().unwrap_or_else(|| "value".to_string());
    let pairs: Vec<(Value, Value)> = arr.iter(|it| {
        it.map(|(k, v)| (Value::input(bytes_of(k)), Value::input(bytes_of(v))))
            .collect()
    });
    // (not `sort_by(awk_cmp)`: it is no total order over numeric and other keys, and `sort_by`
    // may panic on one that is not)
    let pairs = ops::sort_by_value(pairs, |p| &p.0);
    let (keys, vals): (Vec<Value>, Vec<Value>) = pairs.into_iter().unzip();
    let df = DataFrame::from_columns(vec![
        Column::from_values(key_name, None, keys),
        Column::from_values(val_name, None, vals),
    ]);
    Str::from_bytes(&insert(df))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_codes() {
        for op in DfOp::ALL {
            assert_eq!(DfOp::from_code(op.code()), Some(op));
            assert!(op.func_name().starts_with("df_"));
        }
        assert_eq!(DfOp::from_code(-1), None);
        assert_eq!(DfOp::from_code(DfOp::ALL.len() as Int), None);
    }

    #[test]
    fn handles() {
        let h = insert(DataFrame::default());
        assert!(h.starts_with(b"df#"));
        assert!(is_handle(&h));
        assert!(get(&h).is_ok());
        assert!(free(&h));
        assert!(!is_handle(&h));
        let err = get(b"df#x").unwrap_err();
        assert_eq!(err, "invalid dataframe handle \"df#x\"");
        assert!(!is_handle(b"hello"));
    }

    #[test]
    fn append_rows() {
        let h = insert(ops::new("name,age:int").unwrap());
        with_mut(&h, |df| {
            df.push_row(vec![Value::input("Ann"), Value::input("42")]);
            df.push_row(vec![Value::input("Bob")]);
            df.push_row(vec![Value::input("Cy"), Value::input("7"), Value::input("x")]);
        })
        .unwrap();
        let df = get(&h).unwrap();
        assert_eq!(df.nrows, 3);
        assert_eq!(df.names(), ["name", "age", "c3"]);
        assert!(df.columns[1].get(1).is_null());
        assert!(df.columns[2].get(0).is_null());
        free(&h);
        // an extra column never takes the name of another one
        let mut df = ops::new("c2").unwrap();
        df.push_row(vec![Value::input("a"), Value::input("b")]);
        assert_eq!(df.names(), ["c2", "c2_1"]);
        assert_eq!(df.unique_name("x"), "x");
    }
}
