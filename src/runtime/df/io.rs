//! Loading and writing tables: `df_read`, `df_print`, `df_write`, `df_to_csv`, `df_to_json` and
//! `df_describe`.
//!
//! Tables are read with the readers of `zawk inspect` (`inspect::read_rows`: JSON, JSON Lines,
//! Parquet, Avro, Excel, databases, from files, `http(s)://` and `s3://` URLs, compressed or
//! not); CSV and TSV are read here, byte for byte, so that text that is not UTF-8 is kept as is
//! (as `-i csv` keeps it). Tables are written as the TSV stream that the `-o` writers read
//! (`name:TYPE` header, `\N` for a null with `--null`), through those writers: `df_write(d,
//! "out.parquet")` writes the file `-o parquet` writes for the same rows.
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};

use super::DataFrame;
use super::column::{Column, ColData, Kind, Value, infer_text_kind, typed_text_column};
use super::expr::Res;
use crate::inspect::{self, Cell, Format, RowSink};
use crate::runtime::str_escape;

/// The options of `df_read`: `"infer_types=1;header=0;limit=1000;columns=a,b"`.
#[derive(Default)]
struct ReadOptions {
    infer_types: bool,
    no_header: bool,
    limit: Option<u64>,
    columns: Option<String>,
}

fn parse_options(opts: &str) -> Res<ReadOptions> {
    let mut out = ReadOptions::default();
    for item in opts.split(';') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (key, value) = item.split_once('=').unwrap_or((item, "1"));
        let (key, value) = (key.trim(), value.trim());
        let flag = || matches!(value, "1" | "true" | "yes" | "on");
        match key {
            "infer_types" | "infer" => out.infer_types = flag(),
            "header" => out.no_header = !flag(),
            "limit" | "max_rows" => {
                out.limit = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid limit \"{}\"", value))?,
                )
            }
            "columns" | "cols" => out.columns = Some(value.to_string()),
            _ => {
                return Err(format!(
                    "unknown option \"{}\" (expected infer_types, header, limit or columns)",
                    key
                ));
            }
        }
    }
    Ok(out)
}

/// The value of a cell of a reader of `inspect`, as awk sees it: numbers of typed sources stay
/// numbers, text is a strnum.
fn cell_value(cell: Cell) -> Value {
    match cell {
        Cell::Null => Value::Null,
        Cell::Bool(b) => Value::Bool(b),
        Cell::Num(s) => crate::runtime::typed_value::parse_int(&s)
            .map(Value::Int)
            .or_else(|| crate::runtime::typed_value::parse_float(&s).map(Value::Float))
            .unwrap_or_else(|| Value::input(s)),
        Cell::Str(s) | Cell::Nested(s) | Cell::Text(s) => Value::input(s),
    }
}

/// Collects the rows of a reader, column by column.
#[derive(Default)]
struct Sink {
    names: Vec<String>,
    decls: Vec<Option<String>>,
    index: HashMap<String, usize>,
    cols: Vec<Vec<Value>>,
    rows: u64,
}

impl Sink {
    fn column(&mut self, name: String, decl: Option<String>) -> usize {
        let i = self.names.len();
        self.index.entry(name.clone()).or_insert(i);
        // A repeated name (`a,a,b`, or `cN` for an extra field when the header has `c3`) gets a
        // suffix, as `-H` names it (`a_1`): the table looks its columns up by name.
        crate::runtime::splitter::push_unique_name(&mut self.names, &name);
        self.decls.push(decl);
        self.cols.push(vec![Value::Null; self.rows as usize]);
        i
    }

    fn header(&mut self, fields: Vec<String>) {
        for (i, h) in fields.into_iter().enumerate() {
            let h = if i == 0 {
                h.trim_start_matches('\u{feff}').to_string()
            } else {
                h
            };
            let (name, decl) = inspect::split_type(&h);
            let name = if name.is_empty() {
                format!("c{}", i + 1)
            } else {
                name
            };
            self.column(name, decl.map(|d| d.to_ascii_lowercase()));
        }
    }

    fn values(&mut self, values: Vec<Value>) {
        while self.cols.len() < values.len() {
            let n = self.cols.len() + 1;
            self.column(format!("c{}", n), None);
        }
        let mut values = values.into_iter();
        for col in self.cols.iter_mut() {
            col.push(values.next().unwrap_or(Value::Null));
        }
        self.rows += 1;
    }

    fn finish(self, infer_types: bool) -> DataFrame {
        let columns = self
            .names
            .into_iter()
            .zip(self.decls)
            .zip(self.cols)
            .map(|((name, decl), values)| {
                if decl.is_none() && infer_types && values.iter().all(|v| matches!(v, Value::Str(..) | Value::Null)) {
                    let kind = infer_text_kind(&values);
                    typed_text_column(name, kind, values)
                } else {
                    Column::from_values(name, decl, values)
                }
            })
            .collect();
        DataFrame::from_columns(columns)
    }
}

impl RowSink for Sink {
    fn full(&self, max_rows: Option<u64>) -> bool {
        max_rows.is_some_and(|max| self.rows >= max)
    }
    fn add_column(
        &mut self,
        name: String,
        _key: String,
        _declared: Option<String>,
        _field: Option<usize>,
    ) -> usize {
        // The values of typed sources (Parquet, Avro, databases) have their types already.
        self.column(name, None)
    }
    fn add_header(&mut self, fields: Vec<String>) {
        self.header(fields)
    }
    fn add_object(&mut self, pairs: Vec<(String, Cell)>) {
        let mut row = vec![Value::Null; self.cols.len()];
        for (key, cell) in pairs {
            let i = match self.index.get(&key) {
                Some(i) => *i,
                None => self.column(key, None),
            };
            if i >= row.len() {
                row.resize(i + 1, Value::Null);
            }
            row[i] = cell_value(cell);
        }
        self.values(row)
    }
    fn add_row(&mut self, cells: Vec<Cell>) {
        self.values(cells.into_iter().map(cell_value).collect())
    }
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `\t`, `\n`, `\r` and `\\` escapes of a TSV field, on bytes.
pub(super) fn unescape_tsv_bytes(f: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        if f[i] == b'\\' && i + 1 < f.len() {
            match f[i + 1] {
                b't' => out.push(b'\t'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b'\\' => out.push(b'\\'),
                c => {
                    out.push(b'\\');
                    out.push(c);
                }
            }
            i += 2;
        } else {
            out.push(f[i]);
            i += 1;
        }
    }
    out
}

/// Reads CSV or TSV text byte for byte (as `-i csv -H` / `-i tsv -H` split it).
fn read_delimited(
    input: Box<dyn Read + Send>,
    tsv: bool,
    header: bool,
    limit: Option<u64>,
    sink: &mut Sink,
) -> Res<()> {
    let mut header = header;
    let mut row = |fields: Vec<Vec<u8>>, sink: &mut Sink| -> bool {
        if header {
            header = false;
            sink.header(
                fields
                    .iter()
                    .map(|f| String::from_utf8_lossy(f).into_owned())
                    .collect(),
            );
            return true;
        }
        if sink.full(limit) {
            return false;
        }
        sink.values(fields.into_iter().map(Value::input).collect());
        true
    };
    if tsv {
        let mut reader = BufReader::new(input);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line).map_err(|e| e.to_string())? == 0 {
                break;
            }
            let line = trim_eol(&line);
            if line.is_empty() {
                // a blank line is no record, as for CSV (a trailing empty line)
                continue;
            }
            let fields = line
                .split(|b| *b == b'\t')
                .map(unescape_tsv_bytes)
                .collect();
            if !row(fields, sink) {
                break;
            }
        }
    } else {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(input);
        let mut record = csv::ByteRecord::new();
        while reader
            .read_byte_record(&mut record)
            .map_err(|e| e.to_string())?
        {
            if !row(record.iter().map(|f| f.to_vec()).collect(), sink) {
                break;
            }
        }
    }
    Ok(())
}

/// `df_read(path[, format[, options]])`.
pub(crate) fn read(path: &str, format: &str, opts: &str) -> Res<DataFrame> {
    let opts = parse_options(opts)?;
    let header = !opts.no_header;
    let format = format.trim().to_ascii_lowercase();
    let lower = path.to_ascii_lowercase();
    let mut sink = Sink::default();
    let io_err = |e: io::Error| e.to_string();
    let markdown = matches!(format.as_str(), "markdown" | "md")
        || (format.is_empty() && (lower.ends_with(".md") || lower.ends_with(".markdown")));
    let format = if markdown {
        None
    } else if format.is_empty() {
        if crate::runtime::splitter::database::is_db_url(path) {
            Some(Format::Database)
        } else {
            inspect::extension_format(path)
        }
    } else {
        Some(Format::parse(&format).ok_or_else(|| {
            format!(
                "unknown format \"{}\" (expected csv, tsv, json, jsonl, parquet, avro, xlsx, markdown or database)",
                format
            )
        })?)
    };
    if markdown {
        let input: Box<dyn Read + Send> = if path == "-" {
            Box::new(crate::runtime::splitter::markdown::MarkdownCsv::stdin())
        } else {
            Box::new(crate::runtime::splitter::markdown::MarkdownCsv::file(path.to_string()))
        };
        inspect::read_text_rows(input, Format::Csv, true, opts.limit, &mut sink).map_err(io_err)?;
    } else if matches!(format, Some(Format::Csv) | Some(Format::Tsv)) {
        let input = inspect::open_input(path).map_err(io_err)?;
        read_delimited(input, format == Some(Format::Tsv), header, opts.limit, &mut sink)?;
    } else {
        inspect::read_rows(path, format, header, opts.limit, &mut sink).map_err(io_err)?;
    }
    let df = sink.finish(opts.infer_types);
    match &opts.columns {
        Some(cols) => super::ops::select(&df, cols, false),
        None => Ok(df),
    }
}

/// The text of a cell in an output: floats with `OFMT` (as `print` writes them), or in their
/// shortest exact form when `exact` (typed outputs: Parquet, Avro, ...).
fn cell_text(v: &Value, exact: bool) -> Vec<u8> {
    match v {
        Value::Float(f) if exact => crate::runtime::numfmt::float_to_text(*f).into_bytes(),
        v => v.text(true).into_owned(),
    }
}

/// The TSV stream the `-o` writers read: a header (`name:TYPE` when `typed`) and the rows, with
/// `\N` for a null when `--null` is set.
pub(crate) fn tsv_stream(df: &DataFrame, typed: bool, exact: bool) -> Vec<u8> {
    let mut out = tsv_header(df, typed);
    out.extend_from_slice(&tsv_rows(df, exact));
    out
}

/// The header line of [`tsv_stream`].
fn tsv_header(df: &DataFrame, typed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in df.columns.iter().enumerate() {
        if i > 0 {
            out.push(b'\t');
        }
        let name = if typed { c.typed_header() } else { c.name.clone() };
        out.extend_from_slice(str_escape::escape_tsv(&name).as_bytes());
    }
    out.push(b'\n');
    out
}

/// The rows of [`tsv_stream`], after its header.
fn tsv_rows(df: &DataFrame, exact: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let null = str_escape::null_text().is_some();
    for r in 0..df.nrows {
        for (i, c) in df.columns.iter().enumerate() {
            if i > 0 {
                out.push(b'\t');
            }
            let v = c.get(r);
            if v.is_null() && null {
                out.extend_from_slice(str_escape::TSV_NULL.as_bytes());
                continue;
            }
            escape_tsv_bytes(&cell_text(&v, exact), &mut out);
        }
        out.push(b'\n');
    }
    out
}

fn escape_tsv_bytes(bs: &[u8], out: &mut Vec<u8>) {
    for &b in bs {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b => out.push(b),
        }
    }
}

fn csv_field(bs: &[u8], out: &mut Vec<u8>) {
    if crate::runtime::splitter::batch::csv_needs_quotes(bs) {
        out.push(b'"');
        for &b in bs {
            if b == b'"' {
                out.push(b'"');
            }
            out.push(b);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(bs);
    }
}

fn csv_text(df: &DataFrame) -> Vec<u8> {
    let mut out = csv_header(df);
    out.extend_from_slice(&csv_rows(df));
    out
}

/// The header line of [`csv_text`].
fn csv_header(df: &DataFrame) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in df.columns.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        csv_field(c.name.as_bytes(), &mut out);
    }
    out.push(b'\n');
    out
}

/// The rows of [`csv_text`], after its header.
fn csv_rows(df: &DataFrame) -> Vec<u8> {
    let mut out = Vec::new();
    for r in 0..df.nrows {
        for (i, c) in df.columns.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            csv_field(&cell_text(&c.get(r), false), &mut out);
        }
        out.push(b'\n');
    }
    out
}

/// The rows as text separated by `ofs` and terminated by `ors`, after a header line: what
/// `print` writes for each row.
fn ofs_text(df: &DataFrame, ofs: &[u8], ors: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in df.columns.iter().enumerate() {
        if i > 0 {
            out.extend_from_slice(ofs);
        }
        out.extend_from_slice(c.name.as_bytes());
    }
    out.extend_from_slice(ors);
    for r in 0..df.nrows {
        for (i, c) in df.columns.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(ofs);
            }
            out.extend_from_slice(&c.get(r).text(true));
        }
        out.extend_from_slice(ors);
    }
    out
}

/// The table in `format`: csv, tsv, json (an array of objects), jsonl, markdown, table, parquet,
/// avro or xlsx.
pub(crate) fn render(df: &DataFrame, format: &str) -> Res<Vec<u8>> {
    let format = normalize_format(format);
    let format = format.as_str();
    match format {
        "csv" => return Ok(csv_text(df)),
        "tsv" => return Ok(tsv_stream(df, false, false)),
        "json" => {
            let mut out = to_json(df).into_bytes();
            out.push(b'\n');
            return Ok(out);
        }
        "jsonl" | "markdown" | "table" | "parquet" | "avro" | "xlsx" => {}
        f => {
            return Err(format!(
                "unsupported format \"{}\" (expected csv, tsv, json, jsonl, markdown, table, parquet, avro or xlsx)",
                f
            ));
        }
    }
    let typed = matches!(format, "jsonl" | "parquet" | "avro" | "xlsx");
    let tsv = tsv_stream(df, typed, typed);
    let mut out = Vec::new();
    {
        // The writers render (or finish the file) when they are dropped.
        let mut w: Box<dyn Write + '_> = match format {
            "jsonl" => Box::new(crate::runtime::writers::JsonlWriter::new(&mut out, false)),
            "markdown" => Box::new(crate::runtime::writers::MarkdownWriter::new(&mut out, false)),
            "table" => Box::new(crate::runtime::writers::TableWriter::new(&mut out, false)),
            "parquet" => Box::new(crate::runtime::parquet_writer::ParquetWriter::new(&mut out, false)),
            "avro" => Box::new(crate::runtime::avro_writer::AvroWriter::new(&mut out, false)),
            _ => Box::new(crate::runtime::xlsx_writer::XlsxWriter::new(&mut out)),
        };
        w.write_all(&tsv).map_err(|e| e.to_string())?;
        w.flush().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// What `df_print(d[, format])` writes.
pub(crate) enum Printed {
    /// Text written as it is: the table in an explicit format, or without `-o` the fields
    /// separated by `OFS` and the rows terminated by `ORS`, after a header line.
    Text(Vec<u8>),
    /// The table as part of the output of the run (`-o`): stdout is then a single table (CSV with
    /// `-o csv`, else the TSV stream the writer of `-o tsv/jsonl/markdown/table/...` reads) whose
    /// first line is the header, so `header` is written only when it starts stdout.
    Stream { header: Vec<u8>, rows: Vec<u8> },
}

/// What `df_print(d[, format])` writes: see [`Printed`]. With `-o`, a format that is the one of
/// `-o` is the output of the run; another one is written as text with `-o csv` or `-o tsv`, and
/// ignored (with a warning) by the other `-o` formats, whose writer reads stdout as TSV.
pub(crate) fn print_bytes(df: &DataFrame, format: &str, ofs: &[u8], ors: &[u8]) -> Res<Printed> {
    let run_format = super::output_format();
    let format = normalize_format(format);
    if !format.is_empty() && format != run_format {
        if matches!(run_format, "" | "csv" | "tsv") {
            return Ok(Printed::Text(render(df, &format)?));
        }
        // Check the format, then follow `-o`.
        if !matches!(
            format.as_str(),
            "csv" | "tsv" | "json" | "jsonl" | "markdown" | "table" | "parquet" | "avro" | "xlsx"
        ) {
            return render(df, &format).map(Printed::Text);
        }
        crate::runtime::stdlib_warning(
            "df_print",
            format_args!(
                "format \"{}\" ignored: with -o {} stdout is written as {}",
                format, run_format, run_format
            ),
        );
    }
    let typed = matches!(run_format, "jsonl" | "parquet" | "avro" | "xlsx");
    Ok(match run_format {
        "" => Printed::Text(ofs_text(df, ofs, ors)),
        "csv" => Printed::Stream {
            header: csv_header(df),
            rows: csv_rows(df),
        },
        // jsonl, parquet, avro, xlsx: typed columns
        _ => Printed::Stream {
            header: tsv_header(df, typed),
            rows: tsv_rows(df, typed),
        },
    })
}

/// The format of an output file, from its extension (CSV by default).
fn format_of_path(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    let lower = lower.strip_suffix(".gz").unwrap_or(&lower);
    if lower.ends_with(".md") || lower.ends_with(".markdown") {
        return "markdown";
    }
    match inspect::extension_format(lower) {
        Some(Format::Tsv) => "tsv",
        Some(Format::Json) => "json",
        Some(Format::Jsonl) => "jsonl",
        Some(Format::Parquet) => "parquet",
        Some(Format::Avro) => "avro",
        Some(Format::Xlsx) => "xlsx",
        _ => "csv",
    }
}

/// `df_write(d, path[, format])`: writes the table to a file (gzip-compressed for a `.gz` path)
/// or an `s3://` object.
pub(crate) fn write(df: &DataFrame, path: &str, format: &str) -> Res<()> {
    if path.is_empty() {
        return Err("no output path given".to_string());
    }
    let format = if format.trim().is_empty() {
        format_of_path(path)
    } else {
        format
    };
    let mut bytes = render(df, format)?;
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".gz") {
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&bytes).map_err(|e| e.to_string())?;
        bytes = enc.finish().map_err(|e| e.to_string())?;
    } else if [".zst", ".zstd", ".bz2", ".xz", ".lz4"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return Err(format!(
            "cannot write `{}': only .gz compression is supported",
            path
        ));
    }
    if crate::runtime::s3::is_s3_url(path) {
        let (bucket, key) = crate::runtime::s3::parse_s3_url(path)?;
        crate::runtime::s3::put_object_bytes(bucket, key, bytes)
            .map_err(|e| format!("cannot write `{}': {}", path, e))?;
        return Ok(());
    }
    std::fs::write(path, bytes).map_err(|e| format!("cannot write `{}': {}", path, e))
}

fn json_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(f) if f.is_finite() => out.push_str(&crate::runtime::numfmt::float_to_text(*f)),
        Value::Float(_) => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Str(bs, _) => {
            out.push('"');
            out.push_str(&str_escape::escape_json(&String::from_utf8_lossy(bs)));
            out.push('"');
        }
    }
}

/// `df_to_json(d)`: the rows as a JSON array of objects; numbers and booleans of typed columns
/// are JSON numbers and booleans, nulls are `null`.
pub(crate) fn to_json(df: &DataFrame) -> String {
    let mut out = String::from("[");
    for r in 0..df.nrows {
        if r > 0 {
            out.push(',');
        }
        out.push('{');
        for (i, c) in df.columns.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('"');
            out.push_str(&str_escape::escape_json(&c.name));
            out.push_str("\":");
            json_value(&c.get(r), &mut out);
        }
        out.push('}');
    }
    out.push(']');
    out
}

fn fmt_num(f: f64) -> String {
    if f.is_finite() && f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{:.4}", f)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

/// `df_describe(d)`: the schema and a summary of each column (non-null count, nulls, distinct
/// values, min, max, mean and standard deviation of the numbers), as a table.
pub(crate) fn describe(df: &DataFrame) -> String {
    let mut records = vec![
        ["column", "type", "count", "nulls", "distinct", "min", "max", "mean", "std"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    ];
    for c in &df.columns {
        let values: Vec<Value> = (0..df.nrows).map(|i| c.get(i)).collect();
        let present: Vec<&Value> = values.iter().filter(|v| !v.is_missing()).collect();
        let distinct: std::collections::HashSet<Vec<u8>> =
            present.iter().map(|v| v.exact_text().into_owned()).collect();
        let nums: Vec<f64> = present.iter().filter_map(|v| v.cmp_num()).collect();
        let numeric = !present.is_empty() && nums.len() == present.len();
        let (min, max) = if numeric {
            let min = nums.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = nums.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            (fmt_num(min), fmt_num(max))
        } else {
            let min = present.iter().map(|v| v.text(false).into_owned()).min();
            let max = present.iter().map(|v| v.text(false).into_owned()).max();
            let text = |t: Option<Vec<u8>>| t.map_or(String::new(), |t| String::from_utf8_lossy(&t).into_owned());
            (text(min), text(max))
        };
        let (mean, std) = if numeric {
            let mean = crate::runtime::array_stats::stat_of(
                nums.clone(),
                crate::runtime::array_stats::STAT_MEAN,
                0.0,
            );
            let std = crate::runtime::array_stats::stat_of(
                nums,
                crate::runtime::array_stats::STAT_STDDEV,
                0.0,
            );
            (fmt_num(mean), fmt_num(std))
        } else {
            (String::new(), String::new())
        };
        let ty = match (c.kind(), &c.decl) {
            (Kind::Str, None) if numeric => "string (numeric)".to_string(),
            _ => c.type_name(),
        };
        records.push(vec![
            c.name.clone(),
            ty,
            present.len().to_string(),
            (values.len() - present.len()).to_string(),
            distinct.len().to_string(),
            min,
            max,
            mean,
            std,
        ]);
    }
    let mut out = format!("{} rows, {} columns\n", df.nrows, df.columns.len());
    out.push_str(&inspect::boxed_table(records));
    out
}

/// Whether a column holds data of a fixed type (not text): for tests of the readers.
#[allow(dead_code)]
pub(crate) fn is_typed(c: &Column) -> bool {
    !matches!(c.data, ColData::Str(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, content: &[u8]) -> String {
        let dir = std::env::temp_dir().join(format!("zawk-df-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn read_csv_and_tsv() {
        let path = tmp("people.csv", b"name,age:int\nAnn,42\n\"B, b\",\n");
        let df = read(&path, "", "").unwrap();
        assert_eq!(df.names(), ["name", "age"]);
        assert_eq!(df.nrows, 2);
        assert_eq!(df.columns[1].type_name(), "int");
        assert!(df.columns[1].get(1).is_null());
        assert_eq!(&*df.columns[0].get(1).text(false), b"B, b");
        let path = tmp("t.tsv", b"a\tb\nx\\ty\t\xff\n");
        let df = read(&path, "", "").unwrap();
        assert_eq!(&*df.columns[0].get(0).text(false), b"x\ty");
        // bytes that are not UTF-8 are kept
        assert_eq!(&*df.columns[1].get(0).text(false), b"\xff");
        let df = read(&path, "tsv", "header=0;limit=1").unwrap();
        assert_eq!(df.names(), ["c1", "c2"]);
        assert_eq!(df.nrows, 1);
        // repeated names, blank lines
        let path = tmp("dup.tsv", b"a\ta\tc3\nx\ty\tz\tw\n\n");
        let df = read(&path, "", "").unwrap();
        assert_eq!(df.names(), ["a", "a_1", "c3", "c4"]);
        assert_eq!(df.nrows, 1);
        assert_eq!(&*df.columns[1].get(0).text(false), b"y");
        let path = tmp("dup.csv", b"a,a\n1,2\n");
        let df = read(&path, "", "").unwrap();
        assert_eq!(df.names(), ["a", "a_1"]);
    }

    #[test]
    fn infer_types_and_jsonl() {
        let path = tmp("n.csv", b"a,b,c\n1,1.5,x\n2,,y\n");
        let df = read(&path, "csv", "infer_types=1").unwrap();
        assert_eq!(df.columns[0].kind(), Kind::Int);
        assert_eq!(df.columns[1].kind(), Kind::Float);
        assert_eq!(df.columns[2].kind(), Kind::Str);
        let path = tmp("r.jsonl", b"{\"id\":1,\"name\":\"Ann\"}\n{\"id\":2,\"tag\":true}\n");
        let df = read(&path, "", "").unwrap();
        assert_eq!(df.names(), ["id", "name", "tag"]);
        assert_eq!(df.columns[0].kind(), Kind::Int);
        assert!(df.columns[1].get(1).is_null());
        assert!(is_typed(&df.columns[2]));
    }

    #[test]
    fn outputs() {
        let path = tmp("o.csv", b"name,n\nAnn,1\n\"x,y\",2\n");
        let df = read(&path, "", "infer_types=1").unwrap();
        assert_eq!(render(&df, "csv").unwrap(), b"name,n\nAnn,1\n\"x,y\",2\n");
        assert_eq!(render(&df, "tsv").unwrap(), b"name\tn\nAnn\t1\nx,y\t2\n");
        assert_eq!(to_json(&df), r#"[{"name":"Ann","n":1},{"name":"x,y","n":2}]"#);
        assert_eq!(ofs_text(&df, b" ", b"\n"), b"name n\nAnn 1\nx,y 2\n");
        assert_eq!(
            tsv_stream(&df, true, true),
            b"name\tn:int64\nAnn\t1\nx,y\t2\n"
        );
        let jsonl = render(&df, "jsonl").unwrap();
        assert_eq!(
            String::from_utf8_lossy(&jsonl),
            "{\"name\":\"Ann\",\"n\":1}\n{\"name\":\"x,y\",\"n\":2}\n"
        );
        assert!(render(&df, "nope").is_err());
        // write and read back
        let out = tmp("back.parquet", b"");
        write(&df, &out, "").unwrap();
        let back = read(&out, "", "").unwrap();
        assert_eq!(back.names(), ["name", "n"]);
        assert_eq!(back.nrows, 2);
        assert_eq!(&*back.columns[1].get(1).text(false), b"2");
        assert!(describe(&df).contains("2 rows, 2 columns"));
    }
}
