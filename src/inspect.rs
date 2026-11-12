//! `zawk inspect`: a quick look at a CSV, TSV, JSON, JSON Lines, Parquet, Avro or Excel file, or a database
//! table or query (`sqlite://app.db#user`), before writing a program for it.
//!
//! The report has three parts:
//!
//! * the schema: the field number (`$1`), name and type of each column. A CSV/TSV header of
//!   `name:TYPE` declares the type of the column (as for `-o parquet`); other column types are
//!   inferred from the values (Parquet columns have the type of the file schema, and database
//!   columns their type in the database);
//! * a sample: the first rows (10 by default), with values as awk sees them, and the last
//!   10 rows when the input has more than 50 rows;
//! * a summary of every column, like DuckDB's `SUMMARIZE`: min, max, approx_unique, avg, std,
//!   q25, q50, q75, count and null_percentage.
//!
//! The report is a monospaced table (the default) or Markdown.
//!
//! The whole input is read for the summary (unless `--max-rows` is given). Distinct values are
//! counted exactly up to 100,000 values and estimated with HyperLogLog beyond; quantiles are
//! exact up to 100,000 numbers and computed on a uniform sample beyond.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, Cursor, Read};
use std::sync::{Arc, Mutex};

use unicode_width::UnicodeWidthChar;

use crate::runtime::splitter::{avro::AvroJson, database, json, jsonl, parquet::ParquetJson, xlsx};
use crate::runtime::{decompress, http, s3};

/// The number of last rows (or lines) shown.
pub const TAIL_ROWS: usize = 10;
/// The last rows are shown only when the input has more rows than this.
pub const TAIL_MIN_ROWS: u64 = 50;
/// The most bytes of a line kept by `LineTap`: the rest of a longer line is dropped, so that a
/// text without newlines (e.g. a 2GB one-line JSON) is not held in memory.
const MAX_TAP_LINE: usize = 64 * 1024;

/// A value of a column.
pub enum Cell {
    Null,
    Bool(bool),
    /// A number, as its source text.
    Num(String),
    /// A string (JSON or Parquet): it is not inferred to be a number.
    Str(String),
    /// A JSON object or array.
    Nested(String),
    /// A CSV/TSV field: its type is inferred from the text, and an empty field is null.
    Text(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Csv,
    Tsv,
    /// A JSON document, read as JSON Lines (`file.json#$.data` selects the records).
    Json,
    Jsonl,
    Parquet,
    /// An Avro Object Container File.
    Avro,
    /// A database table or query (`sqlite://app.db#user`), read as CSV with a header line.
    Database,
    /// A sheet of an Excel workbook (`book.xlsx#Sheet2`), read as CSV.
    Xlsx,
}

impl Format {
    pub fn parse(name: &str) -> Option<Format> {
        Some(match name {
            "csv" => Format::Csv,
            "tsv" => Format::Tsv,
            "json" => Format::Json,
            "jsonl" | "ndjson" => Format::Jsonl,
            "parquet" => Format::Parquet,
            "avro" => Format::Avro,
            "database" => Format::Database,
            "xlsx" => Format::Xlsx,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::Jsonl => "jsonl",
            Format::Parquet => "parquet",
            Format::Avro => "avro",
            Format::Database => "database",
            Format::Xlsx => "xlsx",
        }
    }

    /// The options to read this format with zawk.
    fn zawk_options(self) -> &'static str {
        match self {
            Format::Csv => "-i csv -H",
            Format::Tsv => "-i tsv -H",
            Format::Json => "-i json",
            Format::Jsonl => "-i jsonl",
            Format::Parquet => "-i parquet",
            Format::Avro => "-i avro",
            // implied by the URL
            Format::Database => "",
            Format::Xlsx => "-i xlsx -H",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Table,
    Markdown,
}

pub struct Options {
    pub format: Option<Format>,
    pub style: Style,
    /// The number of sample rows.
    pub sample_rows: usize,
    /// Stop reading after this many rows.
    pub max_rows: Option<u64>,
    /// The first CSV/TSV line is the header (`-H`); otherwise the columns are `c1`, `c2`...
    pub header: bool,
}

/// Inspect `path` and return the report.
pub fn inspect(path: &str, opts: &Options) -> io::Result<String> {
    let (ins, format, limited) = read(path, opts, None)?;
    Ok(ins.report(path, format, limited, opts.style))
}

/// The parts of the report of `path`, in Markdown, for `zawk llm`.
pub struct Sections {
    pub rows: u64,
    /// Reading stopped at `max_rows`.
    pub limited: bool,
    pub schema: String,
    pub sample: String,
    /// The last rows, when there are more than `TAIL_MIN_ROWS` rows (and all were read).
    pub tail: Option<String>,
    pub summary: String,
    pub notes: Vec<String>,
    /// The first `raw_lines` lines of the (decompressed) text, read from the same stream as the
    /// rows, so a remote input is fetched once; empty for Parquet.
    pub lines: Vec<String>,
    /// The last `TAIL_ROWS` lines of the text, when it has more than `TAIL_MIN_ROWS` lines (and
    /// all were read); empty otherwise.
    pub tail_lines: Vec<String>,
}

pub fn sections(path: &str, opts: &Options, raw_lines: usize) -> io::Result<Sections> {
    let tap = LineTap::new(raw_lines);
    let (ins, format, limited) = read(path, opts, Some(&tap))?;
    let [schema, sample, summary] = ins.tables().map(|t| t.markdown());
    let tail = ins.tail_table(limited).map(|t| t.markdown());
    // As for the rows: only when there are more than `TAIL_MIN_ROWS` rows, all read.
    let tail_lines = if limited || ins.rows <= TAIL_MIN_ROWS {
        Vec::new()
    } else {
        tap.tail(0)
    };
    Ok(Sections {
        rows: ins.rows,
        limited,
        schema,
        sample,
        tail,
        summary,
        notes: ins.notes(format),
        lines: tap.lines(),
        tail_lines,
    })
}

/// The first `head` lines of the text of `path` and its last `TAIL_ROWS` lines (empty unless it
/// has more than `TAIL_MIN_ROWS` lines after the header line, with `header`), read in one pass.
/// With `max_lines`, reading stops after that many lines, and the last lines are not known.
pub fn text_lines(
    path: &str,
    head: usize,
    header: bool,
    max_lines: Option<u64>,
) -> io::Result<(Vec<String>, Vec<String>)> {
    let tap = LineTap::new(head);
    let mut input = tap.wrap(open_input(path)?);
    let mut buf = vec![0u8; 64 * 1024];
    let mut count = 0u64;
    loop {
        let n = match input.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        // the tap keeps the lines; only count them here, without holding a whole line
        count += buf[..n].iter().filter(|b| **b == b'\n').count() as u64;
        if max_lines.is_some_and(|max| count >= max) {
            return Ok((tap.lines(), Vec::new()));
        }
    }
    Ok((tap.lines(), tap.tail(TAIL_MIN_ROWS + u64::from(header))))
}

/// Keeps a copy of the first lines and of the last lines read through the readers it wraps.
struct LineTap {
    state: Arc<Mutex<TapState>>,
    max_lines: usize,
}

#[derive(Default)]
struct TapState {
    /// The first `max_lines` lines, each one cut to `MAX_TAP_LINE` bytes.
    head: Vec<u8>,
    head_lines: usize,
    /// The number of bytes of the head line being read.
    head_line_len: usize,
    /// The last `TAIL_ROWS` complete lines, and the line being read (cut to `MAX_TAP_LINE` bytes).
    tail: VecDeque<Vec<u8>>,
    partial: Vec<u8>,
    /// The number of complete lines read.
    total: u64,
}

impl TapState {
    fn feed(&mut self, bytes: &[u8], max_lines: usize) {
        for piece in bytes.split_inclusive(|b| *b == b'\n') {
            let ends_line = piece.ends_with(b"\n");
            let text = if ends_line {
                &piece[..piece.len() - 1]
            } else {
                piece
            };
            if self.head_lines < max_lines {
                let keep = text.len().min(MAX_TAP_LINE - self.head_line_len);
                self.head.extend_from_slice(&text[..keep]);
                self.head_line_len += keep;
                if ends_line {
                    self.head.push(b'\n');
                    self.head_lines += 1;
                    self.head_line_len = 0;
                }
            }
            let keep = text.len().min(MAX_TAP_LINE - self.partial.len());
            self.partial.extend_from_slice(&text[..keep]);
            if ends_line {
                self.partial.push(b'\n');
                self.push_line();
            }
        }
    }

    fn push_line(&mut self) {
        let line = if self.tail.len() == TAIL_ROWS {
            let mut old = self.tail.pop_front().unwrap_or_default();
            old.clear();
            std::mem::replace(&mut self.partial, old)
        } else {
            std::mem::take(&mut self.partial)
        };
        self.tail.push_back(line);
        self.total += 1;
    }
}

impl LineTap {
    fn new(max_lines: usize) -> LineTap {
        LineTap {
            state: Arc::default(),
            max_lines,
        }
    }

    fn wrap(&self, input: Box<dyn Read + Send>) -> Box<dyn Read + Send> {
        Box::new(Tapped {
            inner: input,
            state: self.state.clone(),
            max_lines: self.max_lines,
        })
    }

    /// Forgets what was read.
    fn clear(&self) {
        *self.state.lock().unwrap() = TapState::default();
    }

    fn lines(&self) -> Vec<String> {
        let state = self.state.lock().unwrap();
        let buf = state.head.strip_suffix(b"\n").unwrap_or(&state.head);
        if buf.is_empty() {
            return Vec::new();
        }
        buf.split(|b| *b == b'\n')
            .take(self.max_lines)
            .map(|l| String::from_utf8_lossy(trim_eol(l)).into_owned())
            .collect()
    }

    /// The last `TAIL_ROWS` lines, when more than `min_lines` lines were read.
    fn tail(&self, min_lines: u64) -> Vec<String> {
        let mut state = self.state.lock().unwrap();
        if !state.partial.is_empty() {
            // the last line has no newline
            state.push_line();
        }
        if state.total <= min_lines {
            return Vec::new();
        }
        state
            .tail
            .iter()
            .map(|l| String::from_utf8_lossy(trim_eol(l)).into_owned())
            .collect()
    }
}

struct Tapped {
    inner: Box<dyn Read + Send>,
    state: Arc<Mutex<TapState>>,
    max_lines: usize,
}

impl Read for Tapped {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        self.state.lock().unwrap().feed(&out[..n], self.max_lines);
        Ok(n)
    }
}

/// Reads `path` into an inspector; returns it with the format and whether reading stopped at
/// `max_rows`. A text input is read through `tap`, when given.
fn read(
    path: &str,
    opts: &Options,
    tap: Option<&LineTap>,
) -> io::Result<(Inspector, Format, bool)> {
    let mut ins = Inspector::new(opts.sample_rows);
    let open = |path: &str| -> io::Result<Box<dyn Read + Send>> {
        let input = open_input(path)?;
        Ok(match tap {
            Some(tap) => tap.wrap(input),
            None => input,
        })
    };
    let (format, limited) = match opts.format {
        None | Some(Format::Database) if database::is_db_url(path) => {
            (Format::Database, read_database(path, opts, &mut ins)?)
        }
        Some(Format::Database) => {
            return Err(io::Error::other(format!(
                "`{}' is not a database URL",
                path
            )));
        }
        Some(Format::Parquet) => (Format::Parquet, read_parquet(path, opts, &mut ins)?),
        Some(Format::Avro) => (Format::Avro, read_avro(path, opts, &mut ins)?),
        Some(Format::Xlsx) => (Format::Xlsx, read_xlsx(path, opts, &mut ins)?),
        Some(format) => {
            let input = open(path)?;
            let input = if format == Format::Json {
                Box::new(json::JsonRecords::new(input, path))
            } else {
                input
            };
            (format, read_text(input, format, opts, &mut ins)?)
        }
        None => {
            if path != "-" && extension_format(path) == Some(Format::Parquet) {
                (Format::Parquet, read_parquet(path, opts, &mut ins)?)
            } else if path != "-" && extension_format(path) == Some(Format::Avro) {
                (Format::Avro, read_avro(path, opts, &mut ins)?)
            } else if path != "-" && extension_format(path) == Some(Format::Xlsx) {
                (Format::Xlsx, read_xlsx(path, opts, &mut ins)?)
            } else {
                // Sniff the content, then read it (the peeked bytes are put back in front).
                let mut input = open(path)?;
                let mut prefix = Vec::new();
                (&mut input).take(8 << 10).read_to_end(&mut prefix)?;
                let format = match extension_format(path) {
                    Some(format) => format,
                    None if prefix.starts_with(b"PAR1") => Format::Parquet,
                    None if prefix.starts_with(b"Obj\x01") => Format::Avro,
                    None => sniff_text(&prefix),
                };
                if format == Format::Parquet {
                    if path == "-" {
                        return Err(io::Error::other(
                            "Parquet on standard input: use `-i parquet`",
                        ));
                    }
                    if let Some(tap) = tap {
                        // The sniffed bytes are binary, not lines.
                        tap.clear();
                    }
                    (format, read_parquet(path, opts, &mut ins)?)
                } else if format == Format::Avro {
                    if let Some(tap) = tap {
                        tap.clear();
                    }
                    let input: Box<dyn Read + Send> = Box::new(Cursor::new(prefix).chain(input));
                    (
                        format,
                        read_avro_from(AvroJson::reader(input, path), opts, &mut ins)?,
                    )
                } else {
                    let input: Box<dyn Read + Send> = Box::new(Cursor::new(prefix).chain(input));
                    let input = if format == Format::Json {
                        Box::new(json::JsonRecords::new(input, path))
                    } else {
                        input
                    };
                    (format, read_text(input, format, opts, &mut ins)?)
                }
            }
        }
    };
    Ok((ins, format, limited))
}

/// The format named by the extension of `path` (after a compression extension).
pub fn extension_format(path: &str) -> Option<Format> {
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    let mut path = path.as_str();
    for ext in [".gz", ".zst", ".zstd", ".bz2", ".xz", ".lz4"] {
        if let Some(p) = path.strip_suffix(ext) {
            path = p;
            break;
        }
    }
    let ext = path.rsplit_once('.')?.1;
    Some(match ext {
        "csv" => Format::Csv,
        "tsv" | "tab" => Format::Tsv,
        "json" => Format::Json,
        "jsonl" | "ndjson" => Format::Jsonl,
        "parquet" | "pq" => Format::Parquet,
        "avro" => Format::Avro,
        "xlsx" | "xlsm" | "xls" | "xlsb" | "ods" => Format::Xlsx,
        _ => return None,
    })
}

fn sniff_text(prefix: &[u8]) -> Format {
    let first = prefix.iter().find(|b| !b.is_ascii_whitespace());
    if first == Some(&b'{') {
        return Format::Jsonl;
    }
    if first == Some(&b'[') {
        return Format::Json;
    }
    let line = prefix.split(|b| *b == b'\n').next().unwrap_or_default();
    let count = |c: u8| line.iter().filter(|b| **b == c).count();
    if count(b'\t') > count(b',') {
        Format::Tsv
    } else {
        Format::Csv
    }
}

/// Opens `path` to be read as `format`: a JSON document is read as JSON Lines.
fn open_format(path: &str, format: Format) -> io::Result<Box<dyn Read + Send>> {
    let input = open_input(path)?;
    Ok(if format == Format::Json {
        Box::new(json::JsonRecords::new(input, path))
    } else {
        input
    })
}

/// Opens a file, URL, S3 object or standard input (`-`), decompressing it if needed. A JSONPath
/// after the file (`file.json#$.data`) is ignored.
fn open_input(path: &str) -> io::Result<Box<dyn Read + Send>> {
    if database::is_db_url(path) {
        return Ok(Box::new(database::DbCsv::new(path.to_string())));
    }
    let path = json::split_path(path).0;
    if path == "-" {
        decompress::maybe_decompress(path, io::stdin())
    } else if s3::is_s3_url(path) {
        decompress::maybe_decompress(path, s3::S3Reader::new(path))
    } else if http::is_http_url(path) {
        http::open(path).and_then(|resp| decompress::maybe_decompress(path, resp))
    } else {
        let file = std::fs::File::open(path)
            .map_err(|e| io::Error::new(e.kind(), format!("cannot open `{}': {}", path, e)))?;
        decompress::maybe_decompress(path, file)
    }
}

/// Reads the rows of a Parquet file; returns whether reading stopped at `--max-rows`.
fn read_parquet(path: &str, opts: &Options, ins: &mut Inspector) -> io::Result<bool> {
    if path != "-" {
        ParquetJson::check(path).map_err(io::Error::other)?;
    }
    let source = if path == "-" {
        ParquetJson::stdin()
    } else {
        ParquetJson::file(path.to_string())
    };
    let mut table = source.table()?;
    for (i, (name, ty)) in table.columns().into_iter().enumerate() {
        ins.add_column(name.clone(), name, Some(ty), Some(i + 1));
    }
    while let Some(row) = table.next_row() {
        if ins.full(opts.max_rows) {
            return Ok(true);
        }
        ins.add_row(row?);
    }
    Ok(false)
}

/// Reads the records of an Avro file; returns whether reading stopped at `--max-rows`.
fn read_avro(path: &str, opts: &Options, ins: &mut Inspector) -> io::Result<bool> {
    if path != "-" {
        AvroJson::check(path).map_err(io::Error::other)?;
    }
    let source = if path == "-" {
        AvroJson::stdin()
    } else {
        AvroJson::file(path.to_string())
    };
    read_avro_from(source, opts, ins)
}

fn read_avro_from(source: AvroJson, opts: &Options, ins: &mut Inspector) -> io::Result<bool> {
    let mut table = source.table()?;
    for (i, (name, ty)) in table.columns().into_iter().enumerate() {
        ins.add_column(name.clone(), name, Some(ty), Some(i + 1));
    }
    while let Some(row) = table.next_row() {
        if ins.full(opts.max_rows) {
            return Ok(true);
        }
        ins.add_row(row?);
    }
    Ok(false)
}

/// Reads the rows of a sheet of an Excel workbook; returns whether reading stopped at `--max-rows`.
fn read_xlsx(path: &str, opts: &Options, ins: &mut Inspector) -> io::Result<bool> {
    let sheet = xlsx::Sheet::open(path)?;
    ins.sheet = Some((sheet.name, sheet.sheets));
    let mut rows = sheet.rows.into_iter();
    if opts.header
        && let Some(header) = rows.next() {
            ins.add_header(header.iter().map(xlsx::cell_text).collect());
        }
    for row in rows {
        if ins.full(opts.max_rows) {
            return Ok(true);
        }
        ins.add_row(row.into_iter().map(xlsx_cell).collect());
    }
    Ok(false)
}

fn xlsx_cell(cell: calamine::Data) -> Cell {
    use calamine::Data;
    match cell {
        Data::Empty => Cell::Null,
        Data::Int(n) => Cell::Num(n.to_string()),
        Data::Float(f) => Cell::Num(crate::runtime::numfmt::float_to_text(f)),
        Data::Bool(b) => Cell::Bool(b),
        Data::String(s) => Cell::Str(s),
        cell => Cell::Str(xlsx::cell_text(&cell)),
    }
}

/// Reads the rows of a database table or query; returns whether reading stopped at `--max-rows`.
fn read_database(url: &str, opts: &Options, ins: &mut Inspector) -> io::Result<bool> {
    let mut table = database::DbTable::open(url)?;
    for (i, c) in table.columns().iter().enumerate() {
        ins.add_column(
            c.name.clone(),
            c.name.clone(),
            c.db_type.clone(),
            Some(i + 1),
        );
    }
    while let Some(row) = table.next_row() {
        if ins.full(opts.max_rows) {
            return Ok(true);
        }
        ins.add_row(row?.into_iter().map(db_cell).collect());
    }
    Ok(false)
}

fn db_cell(value: database::DbValue) -> Cell {
    match value {
        database::DbValue::Null => Cell::Null,
        database::DbValue::Num(s) => Cell::Num(s),
        database::DbValue::Text(s) => Cell::Str(s),
    }
}

/// Reads the rows of a CSV, TSV, JSON or JSON Lines input; returns whether reading stopped at
/// `--max-rows`.
fn read_text(
    input: Box<dyn Read + Send>,
    format: Format,
    opts: &Options,
    ins: &mut Inspector,
) -> io::Result<bool> {
    let lossy = |bs: &[u8]| String::from_utf8_lossy(bs).into_owned();
    match format {
        Format::Csv => {
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_reader(input);
            let mut record = csv::ByteRecord::new();
            let mut header = opts.header;
            while reader
                .read_byte_record(&mut record)
                .map_err(io::Error::other)?
            {
                let fields = record.iter().map(lossy).collect();
                if header {
                    ins.add_header(fields);
                    header = false;
                } else if ins.full(opts.max_rows) {
                    return Ok(true);
                } else {
                    ins.add_row(fields.into_iter().map(Cell::Text).collect());
                }
            }
        }
        Format::Tsv => {
            let mut reader = BufReader::new(input);
            let mut line = Vec::new();
            let mut header = opts.header;
            loop {
                line.clear();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                let line = trim_eol(&line);
                let fields = line
                    .split(|b| *b == b'\t')
                    .map(|f| unescape_tsv(&lossy(f)))
                    .collect();
                if header {
                    ins.add_header(fields);
                    header = false;
                } else if ins.full(opts.max_rows) {
                    return Ok(true);
                } else {
                    ins.add_row(fields.into_iter().map(Cell::Text).collect());
                }
            }
        }
        Format::Json | Format::Jsonl => {
            let mut reader = BufReader::new(input);
            let mut line = Vec::new();
            loop {
                line.clear();
                if reader.read_until(b'\n', &mut line)? == 0 {
                    break;
                }
                let line = trim_eol(&line);
                if line.iter().all(|b| b.is_ascii_whitespace()) {
                    continue;
                }
                if ins.full(opts.max_rows) {
                    return Ok(true);
                }
                match jsonl::object_cells(line) {
                    Some(pairs) => ins.add_object(pairs),
                    None => ins.non_objects += 1,
                }
            }
        }
        Format::Parquet | Format::Avro | Format::Database | Format::Xlsx => unreachable!(),
    }
    Ok(false)
}

/// The first records of an input, for `zawk lint` with an input file.
#[derive(Clone, Debug)]
pub enum Sample {
    /// Lines of text (the header line first, with `-H`), to be split by the program's `FS`.
    Lines(Vec<String>),
    /// Records split by an input format: the `FI` keys (a CSV/TSV header line with `-H`, the
    /// keys of the first JSON object, the Parquet and Avro columns) and the fields of each record, as
    /// awk sees them (a null is "").
    Fields {
        header: Option<Vec<String>>,
        rows: Vec<Vec<String>>,
    },
}

/// Reads the first `max_records` records of `path` as a run reads them: with `format` (`-i`),
/// or as lines of text when it is `None`. With `header` (`-H`), the header line is read as well.
pub fn sample(
    path: &str,
    format: Option<Format>,
    header: bool,
    max_records: usize,
) -> io::Result<Sample> {
    let lossy = |bs: &[u8]| String::from_utf8_lossy(bs).into_owned();
    let lines = |input: Box<dyn Read + Send>, max: usize| -> io::Result<Vec<String>> {
        let mut reader = BufReader::new(input);
        let mut out = Vec::new();
        let mut line = Vec::new();
        while out.len() < max {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            out.push(lossy(trim_eol(&line)));
        }
        Ok(out)
    };
    let wanted = max_records + usize::from(header);
    let mut rows: Vec<Vec<String>> = match format {
        None => return Ok(Sample::Lines(lines(open_input(path)?, wanted)?)),
        Some(Format::Csv) => {
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .from_reader(open_input(path)?);
            let mut record = csv::ByteRecord::new();
            let mut rows = Vec::new();
            while rows.len() < wanted
                && reader
                    .read_byte_record(&mut record)
                    .map_err(io::Error::other)?
            {
                rows.push(record.iter().map(lossy).collect());
            }
            rows
        }
        Some(Format::Tsv) => lines(open_input(path)?, wanted)?
            .iter()
            .map(|l| l.split('\t').map(unescape_tsv).collect())
            .collect(),
        Some(format @ (Format::Json | Format::Jsonl)) => {
            // As for a run, the keys of the first object are the fields, in order.
            let mut keys: Option<Vec<String>> = None;
            let mut rows = Vec::new();
            for line in lines(open_format(path, format)?, usize::MAX)? {
                if rows.len() >= max_records {
                    break;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let pairs = jsonl::object_cells(line.as_bytes()).unwrap_or_default();
                let keys =
                    keys.get_or_insert_with(|| pairs.iter().map(|(k, _)| k.clone()).collect());
                let mut row = vec![String::new(); keys.len()];
                for (k, cell) in &pairs {
                    if let Some(i) = keys.iter().position(|x| x == k) {
                        row[i] = awk_text(cell).to_string();
                    }
                }
                rows.push(row);
            }
            return Ok(Sample::Fields {
                header: Some(keys.unwrap_or_default()),
                rows,
            });
        }
        Some(Format::Xlsx) => xlsx::Sheet::open(path)?
            .rows
            .iter()
            .take(wanted)
            .map(|row| row.iter().map(xlsx::cell_text).collect())
            .collect(),
        Some(Format::Database) => {
            let mut table = database::DbTable::open(path)?;
            let header = table.columns().iter().map(|c| c.name.clone()).collect();
            let mut rows = Vec::new();
            while rows.len() < max_records {
                match table.next_row() {
                    Some(row) => rows.push(row?.iter().map(|v| v.text().to_string()).collect()),
                    None => break,
                }
            }
            return Ok(Sample::Fields {
                header: Some(header),
                rows,
            });
        }
        Some(Format::Avro) => {
            if path != "-" {
                AvroJson::check(path).map_err(io::Error::other)?;
            }
            let source = if path == "-" {
                AvroJson::stdin()
            } else {
                AvroJson::file(path.to_string())
            };
            let mut table = source.table()?;
            let header = table.columns().into_iter().map(|(name, _)| name).collect();
            let mut rows = Vec::new();
            while rows.len() < max_records {
                match table.next_row() {
                    Some(row) => rows.push(row?.iter().map(|c| awk_text(c).to_string()).collect()),
                    None => break,
                }
            }
            return Ok(Sample::Fields {
                header: Some(header),
                rows,
            });
        }
        Some(Format::Parquet) => {
            if path != "-" {
                ParquetJson::check(path).map_err(io::Error::other)?;
            }
            let source = if path == "-" {
                ParquetJson::stdin()
            } else {
                ParquetJson::file(path.to_string())
            };
            let mut table = source.table()?;
            let header = table.columns().into_iter().map(|(name, _)| name).collect();
            let mut rows = Vec::new();
            while rows.len() < max_records {
                match table.next_row() {
                    Some(row) => rows.push(row?.iter().map(|c| awk_text(c).to_string()).collect()),
                    None => break,
                }
            }
            return Ok(Sample::Fields {
                header: Some(header),
                rows,
            });
        }
    };
    // CSV, TSV and xlsx: the header line, with `-H`.
    let header_row = (header && !rows.is_empty()).then(|| {
        let mut h = rows.remove(0);
        if let Some(first) = h.first_mut() {
            *first = first.trim_start_matches('\u{feff}').to_string();
        }
        h
    });
    Ok(Sample::Fields {
        header: header_row,
        rows,
    })
}

fn trim_eol(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// `\t`, `\n`, `\r` and `\\` escapes, as in `-i tsv`.
fn unescape_tsv(s: &str) -> String {
    crate::runtime::str_escape::unescape_tsv(s).into_owned()
}

/// `name:TYPE` → (`name`, `TYPE`).
fn split_type(header: &str) -> (String, Option<String>) {
    if let Some((name, ty)) = header.rsplit_once(':') {
        let ty = ty.trim();
        let valid = ty.starts_with(|c: char| c.is_ascii_alphabetic())
            && ty
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_(), ".contains(c));
        if !name.trim().is_empty() && valid {
            return (name.trim().to_string(), Some(ty.to_ascii_uppercase()));
        }
    }
    (header.to_string(), None)
}

struct Column {
    name: String,
    /// The `FI` key of the column.
    key: String,
    declared: Option<String>,
    /// The field number (`$1`), when it is fixed.
    field: Option<usize>,
    stats: Stats,
}

struct Inspector {
    columns: Vec<Column>,
    index: HashMap<String, usize>,
    rows: u64,
    samples: Vec<Vec<String>>,
    sample_rows: usize,
    /// The last `TAIL_ROWS` rows, formatted only when they are shown.
    tail: VecDeque<Vec<Cell>>,
    /// JSON Lines lines that are not objects.
    non_objects: u64,
    /// For a workbook: the sheet read, and the names of all its sheets.
    sheet: Option<(String, Vec<String>)>,
    rng: Rng,
}

impl Inspector {
    fn new(sample_rows: usize) -> Inspector {
        Inspector {
            columns: Vec::new(),
            index: HashMap::new(),
            rows: 0,
            samples: Vec::new(),
            sample_rows,
            tail: VecDeque::with_capacity(TAIL_ROWS),
            non_objects: 0,
            sheet: None,
            rng: Rng(0x9e37_79b9_7f4a_7c15),
        }
    }

    fn full(&self, max_rows: Option<u64>) -> bool {
        max_rows.is_some_and(|max| self.rows >= max)
    }

    fn add_column(
        &mut self,
        name: String,
        key: String,
        declared: Option<String>,
        field: Option<usize>,
    ) -> usize {
        let i = self.columns.len();
        self.index.entry(key.clone()).or_insert(i);
        // the column is null in the rows read before it appeared
        let stats = Stats {
            nulls: self.rows,
            ..Default::default()
        };
        self.columns.push(Column {
            name,
            key,
            declared,
            field,
            stats,
        });
        i
    }

    fn add_header(&mut self, fields: Vec<String>) {
        for (i, header) in fields.into_iter().enumerate() {
            let header = if i == 0 {
                header.trim_start_matches('\u{feff}').to_string()
            } else {
                header
            };
            let (name, declared) = split_type(&header);
            let name = if name.is_empty() {
                format!("c{}", i + 1)
            } else {
                name
            };
            self.add_column(name, header, declared, Some(i + 1));
        }
    }

    fn add_object(&mut self, pairs: Vec<(String, Cell)>) {
        let first = self.rows == 0 && self.columns.is_empty();
        let mut cells: Vec<Cell> = Vec::with_capacity(self.columns.len());
        cells.resize_with(self.columns.len(), || Cell::Null);
        for (key, cell) in pairs {
            let i = match self.index.get(&key) {
                Some(i) => *i,
                None => {
                    // keys of the first record are the fields $1..$n
                    let field = if first {
                        Some(self.columns.len() + 1)
                    } else {
                        None
                    };
                    self.add_column(key.clone(), key, None, field)
                }
            };
            if i >= cells.len() {
                cells.resize_with(i + 1, || Cell::Null);
            }
            cells[i] = cell;
        }
        self.add_row(cells);
    }

    fn add_row(&mut self, cells: Vec<Cell>) {
        for i in self.columns.len()..cells.len() {
            self.add_column(
                format!("c{}", i + 1),
                format!("c{}", i + 1),
                None,
                Some(i + 1),
            );
        }
        if self.samples.len() < self.sample_rows {
            self.samples.push(sample_row(&cells));
        }
        for (i, column) in self.columns.iter_mut().enumerate() {
            column
                .stats
                .add(cells.get(i).unwrap_or(&Cell::Null), &mut self.rng);
        }
        if self.tail.len() == TAIL_ROWS {
            self.tail.pop_front();
        }
        self.tail.push_back(cells);
        self.rows += 1;
    }

    fn report(&self, path: &str, format: Format, limited: bool, style: Style) -> String {
        let md = style == Style::Markdown;
        let mut out = String::new();
        let rows = if limited {
            format!("{} (stopped at --max-rows)", self.rows)
        } else {
            self.rows.to_string()
        };
        let path = crate::runtime::network::redact_url(path);
        let reader = match format.zawk_options() {
            "" => format!("zawk '...' '{}'", path),
            options => format!("zawk {} '...' {}", options, path),
        };
        if md {
            out.push_str(&format!("# Inspect `{}`\n\n", path));
            out.push_str(&format!(
                "- Format: {} (read with `{}`)\n",
                format.name(),
                reader
            ));
            if let Some(sheets) = self.sheets_line() {
                out.push_str(&format!("- Sheets: {}\n", sheets));
            }
            out.push_str(&format!(
                "- Rows: {}\n- Columns: {}\n",
                rows,
                self.columns.len()
            ));
        } else {
            out.push_str(&format!("File:    {}\n", path));
            out.push_str(&format!(
                "Format:  {} (read with: {})\n",
                format.name(),
                reader
            ));
            if let Some(sheets) = self.sheets_line() {
                out.push_str(&format!("Sheets:  {}\n", sheets));
            }
            out.push_str(&format!(
                "Rows:    {}\nColumns: {}\n",
                rows,
                self.columns.len()
            ));
        }

        let [schema, sample, summary] = self.tables();
        let mut sections = vec![
            ("Schema".to_string(), schema),
            (
                format!("Sample (first {} rows)", self.samples.len()),
                sample,
            ),
        ];
        if let Some(tail) = self.tail_table(limited) {
            sections.push((format!("Sample (last {} rows)", tail.rows.len()), tail));
        }
        sections.push(("Summary".to_string(), summary));
        for (title, table) in &sections {
            if md {
                out.push_str(&format!("\n## {}\n\n", title));
                out.push_str(&table.markdown());
            } else {
                out.push_str(&format!("\n{}:\n", title));
                out.push_str(&table.boxed());
            }
        }

        let notes = self.notes(format);
        if !notes.is_empty() {
            out.push_str(if md { "\n## Notes\n\n" } else { "\nNotes:\n" });
            for note in notes {
                out.push_str(&format!("- {}\n", note));
            }
        }
        out
    }

    /// The sheets of a workbook, the one read marked: `Sheet1 (read), Sheet2`.
    fn sheets_line(&self) -> Option<String> {
        let (name, sheets) = self.sheet.as_ref()?;
        let names: Vec<String> = sheets
            .iter()
            .map(|s| {
                if s == name {
                    format!("{} (read)", s)
                } else {
                    s.clone()
                }
            })
            .collect();
        Some(names.join(", "))
    }

    /// The schema, sample and summary tables.
    fn tables(&self) -> [Table; 3] {
        let types: Vec<String> = self.columns.iter().map(|c| c.data_type()).collect();
        let schema = Table {
            headers: vec!["field".into(), "name".into(), "type".into()],
            right: vec![false; 3],
            rows: self
                .columns
                .iter()
                .zip(&types)
                .map(|(c, ty)| {
                    let field = c.field.map_or("-".to_string(), |i| format!("${}", i));
                    vec![field, c.name.clone(), ty.clone()]
                })
                .collect(),
        };
        let sample = self.rows_table(&types, self.samples.clone());
        let summary_headers = [
            "column_name",
            "column_type",
            "min",
            "max",
            "approx_unique",
            "avg",
            "std",
            "q25",
            "q50",
            "q75",
            "count",
            "null_percentage",
        ];
        let summary = Table {
            headers: summary_headers.iter().map(|s| s.to_string()).collect(),
            right: summary_headers
                .iter()
                .map(|h| !matches!(*h, "column_name" | "column_type" | "min" | "max"))
                .collect(),
            rows: self
                .columns
                .iter()
                .zip(&types)
                .map(|(c, ty)| c.stats.summary(&c.name, ty, self.rows))
                .collect(),
        };

        [schema, sample, summary]
    }

    /// A table of sample rows.
    fn rows_table(&self, types: &[String], mut rows: Vec<Vec<String>>) -> Table {
        for row in rows.iter_mut() {
            row.resize(self.columns.len(), "NULL".to_string());
        }
        Table {
            headers: self.columns.iter().map(|c| c.name.clone()).collect(),
            right: types.iter().map(|t| is_numeric_type(t)).collect(),
            rows,
        }
    }

    /// The last rows, when there are more than `TAIL_MIN_ROWS` rows and all of them were read.
    fn tail_table(&self, limited: bool) -> Option<Table> {
        if limited || self.rows <= TAIL_MIN_ROWS {
            return None;
        }
        let types: Vec<String> = self.columns.iter().map(|c| c.data_type()).collect();
        let rows = self.tail.iter().map(|cells| sample_row(cells)).collect();
        Some(self.rows_table(&types, rows))
    }

    fn notes(&self, format: Format) -> Vec<String> {
        let mut notes = vec![
            "Values are shown as awk sees them: booleans are 1/0, and NULL (null, missing or an empty CSV field) is the empty string.".to_string(),
        ];
        if let Some(c) = self
            .columns
            .iter()
            .find(|c| c.declared.is_some() && matches!(format, Format::Csv | Format::Tsv))
        {
            notes.push(format!(
                "Column types come from `name:TYPE` headers. With -H, `FI` keys are the whole header text, e.g. `$FI[\"{}\"]`, or use the field number.",
                c.key
            ));
        }
        if self.columns.iter().any(|c| c.field.is_none()) {
            notes.push("Columns without a field number are missing from the first record: read them with a constant key, e.g. `$FI[\"key\"]`.".to_string());
        }
        if let Some((name, sheets)) = &self.sheet
            && sheets.len() > 1 {
                notes.push(format!(
                    "The workbook has {} sheets ({}); this is `{}`: read another one with `#NAME` after the file name, e.g. `book.xlsx#{}`.",
                    sheets.len(),
                    sheets.join(", "),
                    name,
                    sheets.iter().find(|s| *s != name).unwrap_or(name)
                ));
            }
        if format == Format::Database {
            notes.push("Database rows are read as CSV with a header line of the column names: `$FI[\"name\"]` or the field number; BLOB values are the empty string.".to_string());
        }
        if self.non_objects > 0 {
            let lines = if self.non_objects == 1 {
                "1 line is".to_string()
            } else {
                format!("{} lines are", self.non_objects)
            };
            notes.push(format!(
                "{} not a JSON object: no fields (NF=0), not counted in the rows.",
                lines
            ));
        }
        if self
            .columns
            .iter()
            .any(|c| c.stats.distinct.exact.is_none())
        {
            notes.push("approx_unique is a HyperLogLog estimate for columns with more than 100000 distinct values.".to_string());
        }
        notes
    }
}

impl Column {
    fn data_type(&self) -> String {
        self.declared
            .clone()
            .unwrap_or_else(|| self.stats.inferred_type().to_string())
    }
}

/// The cells of a row as shown in a sample: a null or empty cell is `NULL`.
fn sample_row(cells: &[Cell]) -> Vec<String> {
    cells
        .iter()
        .map(|cell| match cell {
            Cell::Null => "NULL".to_string(),
            Cell::Text(s) if s.is_empty() => "NULL".to_string(),
            cell => awk_text(cell).to_string(),
        })
        .collect()
}

/// The text of a value as awk sees it.
fn awk_text(cell: &Cell) -> &str {
    match cell {
        Cell::Null => "",
        Cell::Bool(true) => "1",
        Cell::Bool(false) => "0",
        Cell::Num(s) | Cell::Str(s) | Cell::Nested(s) | Cell::Text(s) => s,
    }
}

fn is_numeric_type(t: &str) -> bool {
    let t = t.to_ascii_uppercase();
    if t.starts_with("INTERVAL") {
        return false;
    }
    [
        "INT", "UINT", "BIGINT", "SMALLINT", "TINYINT", "LONG", "FLOAT", "DOUBLE", "REAL",
        "DECIMAL", "NUMERIC",
    ]
    .iter()
    .any(|p| t.starts_with(p))
}

// The shapes of values, for type inference (a bit set).
const BOOL: u8 = 1;
const INT: u8 = 2;
const FLOAT: u8 = 4;
const DATE: u8 = 8;
const TIMESTAMP: u8 = 16;
const TIME: u8 = 32;
const NESTED: u8 = 64;
const OTHER: u8 = 128;

fn text_shape(s: &str) -> u8 {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
    let t = s.trim();
    if t.eq_ignore_ascii_case("true") || t.eq_ignore_ascii_case("false") {
        return BOOL;
    }
    let b = t.as_bytes();
    let numeric_start = match b.first() {
        Some(c) if c.is_ascii_digit() => true,
        Some(b'+' | b'-' | b'.') => b.get(1).is_some_and(|c| c.is_ascii_digit() || *c == b'.'),
        _ => false,
    };
    if numeric_start {
        if t.parse::<i64>().is_ok() {
            return INT;
        }
        if t.parse::<f64>().is_ok() {
            return FLOAT;
        }
    }
    if b.len() == 10 && NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok() {
        return DATE;
    }
    if b.len() >= 16 && b[4] == b'-' {
        let timestamp = [
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%d %H:%M",
            "%Y-%m-%dT%H:%M",
        ]
        .iter()
        .any(|f| NaiveDateTime::parse_from_str(t, f).is_ok());
        if timestamp || DateTime::parse_from_rfc3339(t).is_ok() {
            return TIMESTAMP;
        }
    }
    if (5..=18).contains(&b.len()) && b[2] == b':'
        && (NaiveTime::parse_from_str(t, "%H:%M:%S%.f").is_ok()
            || NaiveTime::parse_from_str(t, "%H:%M").is_ok())
        {
            return TIME;
        }
    OTHER
}

const EXACT_LIMIT: usize = 100_000;
const RESERVOIR: usize = 100_000;

#[derive(Default)]
struct Stats {
    count: u64,
    nulls: u64,
    shapes: u8,
    text_min: Option<String>,
    text_max: Option<String>,
    // numbers
    num_n: u64,
    mean: f64,
    m2: f64,
    num_min: Option<(f64, String)>,
    num_max: Option<(f64, String)>,
    reservoir: Vec<f64>,
    distinct: Distinct,
}

impl Stats {
    fn add(&mut self, cell: &Cell, rng: &mut Rng) {
        let (text, shape) = match cell {
            Cell::Null => {
                self.nulls += 1;
                return;
            }
            Cell::Text(s) if s.is_empty() => {
                self.nulls += 1;
                return;
            }
            Cell::Text(s) => (s.as_str(), text_shape(s)),
            // strings may be dates or times, but not numbers
            Cell::Str(s) => {
                let shape = text_shape(s) & (DATE | TIMESTAMP | TIME);
                (s.as_str(), if shape == 0 { OTHER } else { shape })
            }
            Cell::Num(s) => (
                s.as_str(),
                if s.parse::<i64>().is_ok() { INT } else { FLOAT },
            ),
            Cell::Bool(_) => (awk_text(cell), BOOL),
            Cell::Nested(s) => (s.as_str(), NESTED),
        };
        self.count += 1;
        self.shapes |= shape;
        if shape & (INT | FLOAT) != 0
            && let Ok(v) = text.trim().parse::<f64>()
                && v.is_finite() {
                    self.add_number(v, text, rng);
                }
        if self.text_min.as_deref().is_none_or(|m| text < m) {
            self.text_min = Some(text.to_string());
        }
        if self.text_max.as_deref().is_none_or(|m| text > m) {
            self.text_max = Some(text.to_string());
        }
        self.distinct.insert(text.as_bytes());
    }

    fn add_number(&mut self, v: f64, text: &str, rng: &mut Rng) {
        self.num_n += 1;
        let delta = v - self.mean;
        self.mean += delta / self.num_n as f64;
        self.m2 += delta * (v - self.mean);
        if self.num_min.as_ref().is_none_or(|(m, _)| v < *m) {
            self.num_min = Some((v, text.trim().to_string()));
        }
        if self.num_max.as_ref().is_none_or(|(m, _)| v > *m) {
            self.num_max = Some((v, text.trim().to_string()));
        }
        if self.reservoir.len() < RESERVOIR {
            self.reservoir.push(v);
        } else {
            let j = rng.below(self.num_n);
            if (j as usize) < RESERVOIR {
                self.reservoir[j as usize] = v;
            }
        }
    }

    fn inferred_type(&self) -> &'static str {
        match self.shapes {
            0 => "NULL",
            BOOL => "BOOLEAN",
            INT => "INT64",
            s if s & !(INT | FLOAT) == 0 => "DOUBLE",
            DATE => "DATE",
            s if s & !(DATE | TIMESTAMP) == 0 => "TIMESTAMP",
            TIME => "TIME",
            NESTED => "JSON",
            _ => "STRING",
        }
    }

    /// A row of the summary table.
    fn summary(&self, name: &str, ty: &str, rows: u64) -> Vec<String> {
        let numeric = is_numeric_type(ty) && self.num_n > 0;
        let (min, max) = if numeric {
            (
                self.num_min.as_ref().map(|m| m.1.clone()),
                self.num_max.as_ref().map(|m| m.1.clone()),
            )
        } else {
            (self.text_min.clone(), self.text_max.clone())
        };
        let mut row = vec![
            name.to_string(),
            ty.to_string(),
            min.unwrap_or_default(),
            max.unwrap_or_default(),
            self.distinct.count().to_string(),
        ];
        if numeric {
            let mut sorted = self.reservoir.clone();
            sorted.sort_by(f64::total_cmp);
            let q = |p: f64| fmt_num(sorted[(p * (sorted.len() - 1) as f64).round() as usize]);
            let std = if self.num_n > 1 {
                fmt_num((self.m2 / (self.num_n - 1) as f64).sqrt())
            } else {
                String::new()
            };
            row.extend([fmt_num(self.mean), std, q(0.25), q(0.5), q(0.75)]);
        } else {
            row.extend(std::iter::repeat_n(String::new(), 5));
        }
        row.push(rows.to_string());
        let pct = if rows == 0 {
            0.0
        } else {
            self.nulls as f64 * 100.0 / rows as f64
        };
        row.push(format!("{:.2}%", pct));
        row
    }
}

/// A number in a short form: integers as is, others with up to 4 decimals (6 below 1).
fn fmt_num(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    let a = v.abs();
    if v == v.trunc() && a < 1e15 {
        return format!("{}", v as i64);
    }
    if a >= 1e15 || a < 1e-4 {
        return format!("{:.4e}", v);
    }
    let s = format!("{:.*}", if a >= 1.0 { 4 } else { 6 }, v);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Counts distinct values: exactly up to `EXACT_LIMIT` values, with HyperLogLog beyond.
struct Distinct {
    exact: Option<HashSet<u64>>,
    registers: Vec<u8>,
}

const HLL_BITS: u32 = 12;

impl Default for Distinct {
    fn default() -> Distinct {
        Distinct {
            exact: Some(HashSet::new()),
            registers: vec![0; 1 << HLL_BITS],
        }
    }
}

impl Distinct {
    fn insert(&mut self, value: &[u8]) {
        let h = xxhash_rust::xxh64::xxh64(value, 0);
        let idx = (h >> (64 - HLL_BITS)) as usize;
        let rank = ((h << HLL_BITS) | (1 << (HLL_BITS - 1))).leading_zeros() as u8 + 1;
        if rank > self.registers[idx] {
            self.registers[idx] = rank;
        }
        if let Some(set) = &mut self.exact {
            set.insert(h);
            if set.len() > EXACT_LIMIT {
                self.exact = None;
            }
        }
    }

    fn count(&self) -> u64 {
        if let Some(set) = &self.exact {
            return set.len() as u64;
        }
        let m = self.registers.len() as f64;
        let sum: f64 = self.registers.iter().map(|r| 2f64.powi(-(*r as i32))).sum();
        let estimate = 0.7213 / (1.0 + 1.079 / m) * m * m / sum;
        let zeros = self.registers.iter().filter(|r| **r == 0).count();
        if estimate <= 2.5 * m && zeros > 0 {
            (m * (m / zeros as f64).ln()).round() as u64
        } else {
            estimate.round() as u64
        }
    }
}

/// xorshift64, for reservoir sampling.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }
}

struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    /// Right-aligned columns.
    right: Vec<bool>,
}

const TABLE_CELL_WIDTH: usize = 40;
const MARKDOWN_CELL_WIDTH: usize = 64;

/// A value on one line, cut to `max` display columns.
fn cell_text(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let text = match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c => c.to_string(),
        };
        let w = display_width(&text);
        // keep a column for the ellipsis when more text follows
        let room = if chars.peek().is_some() { max - 1 } else { max };
        if width + w > room {
            out.push('…');
            break;
        }
        width += w;
        out.push_str(&text);
    }
    out
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn pad(s: &str, width: usize, right: bool) -> String {
    let fill = " ".repeat(width.saturating_sub(display_width(s)));
    if right {
        format!("{}{}", fill, s)
    } else {
        format!("{}{}", s, fill)
    }
}

/// Renders records as a table with box-drawing borders (`-o table`): the first record is the
/// header, columns whose non-empty values are all numbers are right-aligned, and short records
/// are padded with empty cells. Cells are not cut.
pub fn boxed_table(mut records: Vec<Vec<String>>) -> String {
    if records.is_empty() {
        return String::new();
    }
    let columns = records.iter().map(|r| r.len()).max().unwrap_or(0).max(1);
    for r in records.iter_mut() {
        r.resize(columns, String::new());
    }
    let headers = records.remove(0);
    let right = (0..columns)
        .map(|i| {
            let mut values = records
                .iter()
                .map(|r| r[i].as_str())
                .filter(|s| !s.is_empty())
                .peekable();
            values.peek().is_some() && values.all(|s| matches!(text_shape(s), INT | FLOAT))
        })
        .collect();
    Table {
        headers,
        rows: records,
        right,
    }
    .boxed_with(usize::MAX)
}

impl Table {
    fn cells(&self, max: usize, escape_pipes: bool) -> (Vec<String>, Vec<Vec<String>>) {
        let fix = |s: &String| {
            let s = cell_text(s, max);
            if escape_pipes {
                s.replace('|', "\\|")
            } else {
                s
            }
        };
        let headers = self.headers.iter().map(fix).collect();
        let rows = self
            .rows
            .iter()
            .map(|r| r.iter().map(fix).collect())
            .collect();
        (headers, rows)
    }

    /// A table with box-drawing borders, as DuckDB prints them.
    fn boxed(&self) -> String {
        self.boxed_with(TABLE_CELL_WIDTH)
    }

    fn boxed_with(&self, max: usize) -> String {
        let (headers, rows) = self.cells(max, false);
        let widths: Vec<usize> = (0..headers.len())
            .map(|i| {
                rows.iter()
                    .map(|r| display_width(&r[i]))
                    .chain([display_width(&headers[i])])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rule = |l: &str, m: &str, r: &str| {
            let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
            format!("{}{}{}\n", l, parts.join(m), r)
        };
        let line = |cells: &[String], align: bool| {
            let parts: Vec<String> = cells
                .iter()
                .enumerate()
                .map(|(i, c)| format!(" {} ", pad(c, widths[i], align && self.right[i])))
                .collect();
            format!("│{}│\n", parts.join("│"))
        };
        let mut out = rule("┌", "┬", "┐");
        out.push_str(&line(&headers, false));
        out.push_str(&rule("├", "┼", "┤"));
        for row in &rows {
            out.push_str(&line(row, true));
        }
        out.push_str(&rule("└", "┴", "┘"));
        out
    }

    fn markdown(&self) -> String {
        let (headers, rows) = self.cells(MARKDOWN_CELL_WIDTH, true);
        let mut out = format!("| {} |\n", headers.join(" | "));
        let delims: Vec<&str> = self
            .right
            .iter()
            .map(|r| if *r { "---:" } else { "---" })
            .collect();
        out.push_str(&format!("| {} |\n", delims.join(" | ")));
        for row in &rows {
            out.push_str(&format!("| {} |\n", row.join(" | ")));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_tap_cuts_long_lines() {
        let mut state = TapState::default();
        let chunk = vec![b'x'; 8192];
        // a line without newlines, much longer than MAX_TAP_LINE
        for _ in 0..(4 * MAX_TAP_LINE / chunk.len()) {
            state.feed(&chunk, 3);
        }
        assert_eq!(state.partial.len(), MAX_TAP_LINE);
        assert_eq!(state.head.len(), MAX_TAP_LINE);
        state.feed(b"yy\nab\ncd", 3);
        assert_eq!(state.total, 2);
        assert_eq!(state.tail[0].len(), MAX_TAP_LINE + 1);
        assert_eq!(state.tail[1], b"ab\n");
        assert_eq!(state.partial, b"cd");
        assert_eq!(state.head.len(), MAX_TAP_LINE + 1 + 3 + 2);
        assert_eq!(state.head_lines, 2);
    }

    #[test]
    fn tail_rows_are_the_last_rows() {
        let mut ins = Inspector::new(2);
        for i in 0..100 {
            let mut cells = vec![Cell::Num(i.to_string()), Cell::Text(String::new())];
            if i == 99 {
                cells.push(Cell::Str("z".to_string()));
            }
            ins.add_row(cells);
        }
        assert_eq!(ins.samples, vec![vec!["0", "NULL"], vec!["1", "NULL"]]);
        let tail = ins.tail_table(false).unwrap();
        assert_eq!(tail.rows.len(), TAIL_ROWS);
        assert_eq!(tail.rows[0], vec!["90", "NULL", "NULL"]);
        assert_eq!(tail.rows[9], vec!["99", "NULL", "z"]);
    }

    #[test]
    fn shapes() {
        assert_eq!(text_shape("42"), INT);
        assert_eq!(text_shape("-3.5"), FLOAT);
        assert_eq!(text_shape("1e3"), FLOAT);
        assert_eq!(text_shape("TRUE"), BOOL);
        assert_eq!(text_shape("2024-01-15"), DATE);
        assert_eq!(text_shape("2024-01-15 10:30:00"), TIMESTAMP);
        assert_eq!(text_shape("2024-01-15T10:30:00Z"), TIMESTAMP);
        assert_eq!(text_shape("10:30"), TIME);
        assert_eq!(text_shape("inf"), OTHER);
        assert_eq!(text_shape("abc"), OTHER);
    }

    #[test]
    fn header_types() {
        assert_eq!(
            split_type("age:INT64"),
            ("age".to_string(), Some("INT64".to_string()))
        );
        assert_eq!(
            split_type("price:decimal(10,2)"),
            ("price".to_string(), Some("DECIMAL(10,2)".to_string()))
        );
        assert_eq!(split_type("name"), ("name".to_string(), None));
        assert_eq!(split_type("url:"), ("url:".to_string(), None));
    }

    #[test]
    fn distinct_counts() {
        let mut d = Distinct::default();
        for i in 0..1000 {
            d.insert(format!("{}", i % 300).as_bytes());
        }
        assert_eq!(d.count(), 300);
        let mut d = Distinct::default();
        for i in 0..300_000 {
            d.insert(format!("{}", i).as_bytes());
        }
        let n = d.count() as f64;
        assert!((n - 300_000.0).abs() / 300_000.0 < 0.05, "{}", n);
    }

    #[test]
    fn cells_are_cut() {
        assert_eq!(cell_text("a\tb", 10), "a\\tb");
        assert_eq!(cell_text("abcdef", 4), "abc…");
        assert_eq!(cell_text("abcd", 4), "abcd");
        assert_eq!(cell_text("你好世界", 5), "你好…");
    }

    #[test]
    fn numbers() {
        assert_eq!(fmt_num(3.0), "3");
        assert_eq!(fmt_num(27.8), "27.8");
        assert_eq!(fmt_num(1.0 / 3.0), "0.333333");
        assert_eq!(fmt_num(17.0 / 3.0), "5.6667");
    }
}
