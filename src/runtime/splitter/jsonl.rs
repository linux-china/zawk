//! JSON Lines (`-i jsonl`) input support.
//!
//! Records are newline-delimited JSON values. Each object is projected onto a fixed list of
//! column names (the "schema"): `$1..$NF` hold the values in schema order and `FI` maps each name
//! to its column index.
//!
//! The schema is the key order of the first record, followed by any extra names supplied up front
//! (zawk passes the string constants used as `FI["..."]` keys in the program, so keys that are
//! missing from the first record still get a column). To reuse the `-H`/`FI` machinery, the
//! reader first emits a virtual header record whose fields are the schema names; the first record
//! is then emitted again as ordinary data, so no input is consumed by the header.
//!
//! A key first seen in a later record is appended to the schema (so NF grows from that record
//! on), and reported through `LineReader::new_columns` so that it is added to `FI` as well.
//!
//! Value mapping: strings are unescaped, numbers keep their source text, `true`/`false` become
//! `1`/`0`, `null` and missing keys are empty, and nested objects/arrays keep their raw JSON text.
//! A top-level array is split positionally into `$1..$n`. Blank lines are skipped, and so is a
//! line that is not valid JSON, after a warning on stderr: neither becomes a record.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use crate::common::{CancelSignal, ExecutionStrategy, Result};
use crate::pushdown::FieldSet;
use crate::runtime::{
    str_impl::Str,
    Int, RegexCache,
};

use super::{
    batch::ByteReader,
    chunk::{ChunkProducer, OffsetChunk},
    display_name, input_warning, field_from_end_offset, normalize_join_indexes, resolve_negative_col, DefaultLine, Line, LineReader, ReaderState,
};

// NUL never appears in valid JSON text, so the inner reader never splits a record into fields.
const NO_FIELD_SEP: u8 = 0;

type Inner = ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>>;

#[derive(Default, Clone)]
struct Schema {
    names: Vec<String>,
    index: HashMap<Vec<u8>, usize>,
    // The number of columns in the header. The used-field set only covers those: a later column
    // can still be referenced through FI, so it is always parsed.
    header_len: usize,
}

impl Schema {
    fn push(&mut self, name: String) {
        if !self.index.contains_key(name.as_bytes()) {
            self.index.insert(name.as_bytes().to_vec(), self.names.len());
            self.names.push(name);
        }
    }
}

// The schema shared by the main reader and the workers of a parallel run. It only grows, so a
// column index never changes once assigned.
#[derive(Default)]
struct SharedSchema {
    schema: RwLock<Schema>,
    len: AtomicUsize,
}

impl SharedSchema {
    // Brings `local` up to date, first adding `name` if given and missing.
    fn sync(&self, local: &mut Schema, name: Option<String>) {
        if let Some(name) = name {
            let mut schema = self.schema.write().unwrap();
            schema.push(name);
            self.len.store(schema.names.len(), Ordering::Release);
            if schema.names.len() > local.names.len() {
                *local = schema.clone();
            }
        } else if self.len.load(Ordering::Acquire) > local.names.len() {
            *local = self.schema.read().unwrap().clone();
        }
    }
}

enum Header {
    // Nothing has been read yet.
    Start,
    // The virtual header was returned; the first record (if any) is still to be returned.
    Emitted(Option<Str<'static>>),
    Done,
}

#[derive(Default, Clone)]
pub struct JsonlLine {
    line: Str<'static>,
    fields: Vec<Str<'static>>,
    // A column was assigned, so $0 must be rebuilt from the fields.
    diverged: bool,
    // A column or NF was assigned since the record was read or `$0` was assigned; unlike
    // `diverged`, this stays set after `$0` is rebuilt (see `Line::record`).
    assigned: bool,
}

impl<'a> Line<'a> for JsonlLine {
    fn join_cols<F>(
        &mut self,
        start: Int,
        end: Int,
        sep: &Str<'a>,
        nf: usize,
        trans: F,
    ) -> Result<Str<'a>>
    where
        F: FnMut(Str<'static>) -> Str<'static>,
    {
        let (start, end) = normalize_join_indexes(start, end, nf)?;
        Ok(sep
            .clone()
            .unmoor()
            .join(self.fields[start..end].iter().cloned().map(trans))
            .upcast())
    }
    fn nf(&mut self, _pat: &Str, _rc: &mut RegexCache) -> Result<usize> {
        Ok(self.fields.len())
    }
    fn get_col(&mut self, col: Int, _pat: &Str, ofs: &Str, _rc: &mut RegexCache) -> Result<Str<'a>> {
        let col = if col < 0 {
            match resolve_negative_col(col, self.fields.len())? {
                Some(col) => col,
                None => return Ok(Str::default()),
            }
        } else {
            col
        };
        let res = if col == 0 {
            if self.diverged {
                self.line = ofs.join_slice(&self.fields[..]).unmoor();
                self.diverged = false;
            }
            self.line.clone()
        } else {
            self.fields
                .get((col - 1) as usize)
                .cloned()
                .unwrap_or_default()
        };
        Ok(res.upcast())
    }
    fn set_col(&mut self, col: Int, s: &Str<'a>, _pat: &Str, _rc: &mut RegexCache) -> Result<()> {
        let col = if col < 0 {
            match resolve_negative_col(col, self.fields.len())? {
                Some(c) => c,
                None => {
                    return err!(
                        "attempt to assign field $(-{}); only {} fields",
                        field_from_end_offset(col),
                        self.fields.len()
                    )
                }
            }
        } else {
            col
        };
        if col == 0 {
            self.line = s.clone().unmoor();
            self.diverged = false;
            self.assigned = false;
            return Ok(());
        }
        let col = col as usize - 1;
        if col >= self.fields.len() {
            self.fields.resize_with(col + 1, Str::default);
        }
        self.fields[col] = s.clone().unmoor();
        self.diverged = true;
        self.assigned = true;
        Ok(())
    }
    fn set_nf(&mut self, nf: Int, _pat: &Str, _rc: &mut RegexCache) -> Result<()> {
        if nf < 0 {
            return err!("NF set to negative value {}", nf);
        }
        if nf == 0 {
            self.line = Str::default();
            self.fields.clear();
            self.diverged = false;
            self.assigned = false;
            return Ok(());
        }
        self.fields.resize_with(nf as usize, Str::default);
        self.diverged = true;
        self.assigned = true;
        Ok(())
    }
    fn fields_assigned(&self) -> bool {
        self.assigned
    }
}

pub struct JsonlReader {
    inner: Inner,
    scratch: DefaultLine,
    // Shared with the worker readers of a parallel run, which may be created before the main
    // reader sees the first record.
    shared: Arc<SharedSchema>,
    // This reader's copy of the schema, refreshed when the shared one grows.
    schema: Schema,
    // The number of columns already in FI (the header, or what `new_columns` reported).
    reported: usize,
    extra_names: Vec<String>,
    header: Header,
    used_fields: FieldSet,
    // Line number of the last line read in the current file, for warnings. None for the workers
    // of a parallel run, which do not see whole files.
    line_no: Option<usize>,
    // A bare JSON string or number is a record of plain text: `$0` is the value (`-i json`).
    scalar_text: bool,
}

impl JsonlReader {
    pub fn new<I, S>(
        rs: I,
        extra_names: Vec<String>,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + Send + 'static,
        S: std::io::Read + Send + 'static,
    {
        let inner = ByteReader::new(
            rs,
            NO_FIELD_SEP,
            b'\n',
            chunk_size,
            check_utf8,
            exec_strategy,
            cancel_signal,
        );
        JsonlReader {
            inner,
            scratch: DefaultLine::default(),
            shared: Default::default(),
            schema: Schema::default(),
            reported: 0,
            extra_names,
            header: Header::Start,
            used_fields: FieldSet::all(),
            line_no: Some(0),
            scalar_text: false,
        }
    }

    /// Reads a bare JSON string or number as plain text: `$0` is the (unescaped) value, as `$1`.
    pub fn scalar_text(mut self) -> Self {
        self.scalar_text = true;
        self
    }

    fn build_schema(&mut self, line: &Str<'static>) {
        let mut schema = Schema::default();
        line.with_bytes(|bs| {
            parse_object(bs, |key, _| {
                schema.push(key.into_owned());
            });
        });
        for name in self.extra_names.drain(..) {
            schema.push(name);
        }
        schema.header_len = schema.names.len();
        self.reported = schema.names.len();
        self.shared.len.store(schema.names.len(), Ordering::Release);
        *self.shared.schema.write().unwrap() = schema.clone();
        self.schema = schema;
    }

    fn header_line(&self, out: &mut JsonlLine) {
        let names = &self.schema.names;
        out.fields.clear();
        out.fields
            .extend(names.iter().map(|n| Str::from(n.clone()).unmoor()));
        out.line = Str::from(names.join(",")).unmoor();
        out.diverged = false;
        out.assigned = false;
    }

    /// Split `line` into `out`. Returns false if the line is neither blank nor valid JSON.
    fn fill(&mut self, line: Str<'static>, out: &mut JsonlLine) -> bool {
        out.diverged = false;
        out.assigned = false;
        out.fields.clear();
        let shared = &self.shared;
        let schema = &mut self.schema;
        shared.sync(schema, None);
        let used = &self.used_fields;
        let mut scalar = false;
        let valid = line.with_bytes(|bs| {
            let start = skip_ws(bs, 0);
            match bs.get(start) {
                Some(b'{') => {
                    out.fields.resize_with(schema.names.len(), Str::default);
                    let fields = &mut out.fields;
                    parse_object(bs, |key, val| {
                        let ix = match schema.index.get(key.as_bytes()) {
                            Some(&ix) => ix,
                            None => {
                                // A key not seen before: it gets the next column.
                                let key = key.into_owned();
                                shared.sync(schema, Some(key.clone()));
                                fields.resize_with(schema.names.len(), Str::default);
                                schema.index[key.as_bytes()]
                            }
                        };
                        if ix >= schema.header_len || used.get(ix + 1) {
                            fields[ix] = val.to_str(&line, bs);
                        }
                    })
                }
                Some(b'[') => {
                    let fields = &mut out.fields;
                    parse_array(bs, |val| fields.push(val.to_str(&line, bs)))
                }
                Some(_) => match scan_scalar(bs, start) {
                    Some(val) => {
                        out.fields.push(val.to_str(&line, bs));
                        scalar = true;
                        true
                    }
                    None => false,
                },
                None => true,
            }
        });
        out.line = if scalar && self.scalar_text { out.fields[0].clone() } else { line };
        valid
    }

    fn warn_invalid(&self) {
        static INVALID: AtomicUsize = AtomicUsize::new(0);
        let name = display_name(&self.filename().to_string()).to_string();
        let loc = match self.line_no {
            Some(n) => format!("{}:{}", name, n),
            None => name,
        };
        crate::runtime::set_errno(format!("{}: invalid JSON", loc));
        input_warning(&INVALID, || loc, "invalid JSON");
    }

    /// Reads the next line that is not blank, or None at the end of the input. The flag is
    /// whether a new file was started on the way.
    fn read_nonblank(&mut self, pat: &Str, rc: &mut RegexCache) -> Result<(bool, Option<Str<'static>>)> {
        let mut changed = false;
        loop {
            let file_changed = self.inner.read_line_reuse(pat, rc, &mut self.scratch)?;
            changed |= file_changed;
            if self.inner.read_state() != ReaderState::OK as i64 {
                return Ok((changed, None));
            }
            self.line_no = self.line_no.map(|n| if file_changed { 1 } else { n + 1 });
            let line = trim_cr(self.scratch.line.clone());
            if !line.with_bytes(|bs| skip_ws(bs, 0) == bs.len()) {
                return Ok((changed, Some(line)));
            }
        }
    }

    /// Reads the next valid record into `out`, skipping blank lines and (with a warning) lines
    /// that are not valid JSON. At the end of the input, `out` is left empty.
    fn next_record(&mut self, pat: &Str, rc: &mut RegexCache, out: &mut JsonlLine) -> Result<bool> {
        let mut changed = false;
        loop {
            let (file_changed, line) = self.read_nonblank(pat, rc)?;
            changed |= file_changed;
            let Some(line) = line else {
                self.fill(Str::default(), out);
                return Ok(changed);
            };
            if self.fill(line, out) {
                return Ok(changed);
            }
            self.warn_invalid();
        }
    }
}

impl LineReader for JsonlReader {
    type Line = JsonlLine;
    fn filename(&self) -> Str<'static> {
        self.inner.filename()
    }
    fn wait(&self) -> bool {
        self.inner.wait()
    }
    fn check_utf8(&self) -> bool {
        self.inner.check_utf8()
    }
    fn request_handles(&self, size: usize) -> Vec<Box<dyn FnOnce() -> Self + Send>> {
        self.inner
            .request_handles(size)
            .into_iter()
            .map(|factory| {
                let shared = self.shared.clone();
                let used_fields = self.used_fields.clone();
                let scalar_text = self.scalar_text;
                Box::new(move || JsonlReader {
                    inner: factory(),
                    scratch: DefaultLine::default(),
                    schema: Schema::default(),
                    shared,
                    // A worker's FI may not hold the header yet: report every column.
                    reported: 0,
                    extra_names: Vec::new(),
                    header: Header::Done,
                    used_fields,
                    line_no: None,
                    scalar_text,
                }) as _
            })
            .collect()
    }
    fn read_line(&mut self, pat: &Str, rc: &mut RegexCache) -> Result<(bool, JsonlLine)> {
        let mut line = JsonlLine::default();
        let changed = self.read_line_reuse(pat, rc, &mut line)?;
        Ok((changed, line))
    }
    fn read_line_reuse<'a, 'b: 'a>(
        &'b mut self,
        pat: &Str,
        rc: &mut RegexCache,
        old: &'a mut JsonlLine,
    ) -> Result<bool> {
        match std::mem::replace(&mut self.header, Header::Done) {
            Header::Start => {
                // The schema comes from the first valid record.
                let mut changed = false;
                let first = loop {
                    let (file_changed, line) = self.read_nonblank(pat, rc)?;
                    changed |= file_changed;
                    match line {
                        Some(line) if !line.with_bytes(is_valid) => self.warn_invalid(),
                        line => break line,
                    }
                };
                self.build_schema(first.as_ref().unwrap_or(&Str::default()));
                self.header_line(old);
                self.header = Header::Emitted(first);
                Ok(changed)
            }
            Header::Emitted(Some(first)) => {
                self.fill(first, old);
                Ok(false)
            }
            Header::Emitted(None) | Header::Done => self.next_record(pat, rc, old),
        }
    }
    fn read_state(&self) -> i64 {
        match self.header {
            Header::Emitted(Some(_)) => ReaderState::OK as i64,
            _ => self.inner.read_state(),
        }
    }
    fn next_file(&mut self) -> Result<bool> {
        self.inner.next_file()
    }
    fn set_used_fields(&mut self, used_fields: &FieldSet) {
        self.used_fields = used_fields.clone();
    }
    fn new_columns(&mut self, f: &mut dyn FnMut(Str<'static>, Int)) {
        if self.reported < self.schema.names.len() {
            for (ix, name) in self.schema.names.iter().enumerate().skip(self.reported) {
                f(Str::from(name.clone()).unmoor(), ix as Int + 1);
            }
            self.reported = self.schema.names.len();
        }
    }
}

// Tolerate CRLF line endings.
fn trim_cr(line: Str<'static>) -> Str<'static> {
    let len = line.len();
    if len > 0 && line.with_bytes(|bs| bs[len - 1] == b'\r') {
        line.slice(0, len - 1)
    } else {
        line
    }
}

// A minimal, allocation-free scanner over a single JSON value. It locates the top-level
// keys/values of a record without building a DOM, so projected-out columns cost almost nothing.

enum Val {
    // Span includes the surrounding quotes.
    Str { start: usize, end: usize, escaped: bool },
    // Numbers, nested objects and arrays: kept as their source text.
    Raw { start: usize, end: usize },
    Null,
    True,
    False,
}

impl Val {
    fn to_str(&self, line: &Str<'static>, bs: &[u8]) -> Str<'static> {
        match *self {
            Val::Str { start, end, escaped: false } => line.slice(start + 1, end - 1),
            Val::Str { start, end, escaped: true } => {
                match serde_json::from_slice::<String>(&bs[start..end]) {
                    Ok(s) => Str::from(s).unmoor(),
                    Err(_) => line.slice(start + 1, end - 1),
                }
            }
            Val::Raw { start, end } => line.slice(start, end),
            Val::Null => Str::default(),
            Val::True => Str::from("1"),
            Val::False => Str::from("0"),
        }
    }
}

fn skip_ws(bs: &[u8], mut i: usize) -> usize {
    while i < bs.len() && matches!(bs[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

// `bs[i]` is an opening quote; returns the index just past the closing quote.
fn scan_string(bs: &[u8], mut i: usize) -> Option<(usize, bool)> {
    let mut escaped = false;
    i += 1;
    while i < bs.len() {
        match bs[i] {
            b'"' => return Some((i + 1, escaped)),
            b'\\' => {
                escaped = true;
                i += 2;
            }
            _ => i += 1,
        }
    }
    None
}

// A bare scalar line: only a complete JSON string, number or literal is accepted.
fn scan_scalar(bs: &[u8], start: usize) -> Option<Val> {
    let (val, end) = scan_value(bs, start)?;
    let valid = skip_ws(bs, end) == bs.len()
        && match val {
            Val::Raw { start, end } => std::str::from_utf8(&bs[start..end]).is_ok_and(|s| s.parse::<f64>().is_ok()),
            _ => true,
        };
    valid.then_some(val)
}

// Whether a non-blank line is one record of valid JSON, as `JsonlReader::fill` accepts it.
fn is_valid(bs: &[u8]) -> bool {
    let start = skip_ws(bs, 0);
    match bs.get(start) {
        Some(b'{') => parse_object(bs, |_, _| {}),
        Some(b'[') => parse_array(bs, |_| {}),
        Some(_) => scan_scalar(bs, start).is_some(),
        None => true,
    }
}

fn scan_value(bs: &[u8], i: usize) -> Option<(Val, usize)> {
    let start = i;
    match *bs.get(i)? {
        b'"' => {
            let (end, escaped) = scan_string(bs, i)?;
            Some((Val::Str { start, end, escaped }, end))
        }
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = i;
            while i < bs.len() {
                match bs[i] {
                    b'"' => {
                        i = scan_string(bs, i)?.0;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some((Val::Raw { start, end: i + 1 }, i + 1));
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            None
        }
        _ => {
            let mut end = i;
            while end < bs.len() && !matches!(bs[end], b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n') {
                end += 1;
            }
            let val = match &bs[start..end] {
                b"null" => Val::Null,
                b"true" => Val::True,
                b"false" => Val::False,
                b"" => return None,
                _ => Val::Raw { start, end },
            };
            Some((val, end))
        }
    }
}

enum Key<'a> {
    Plain(&'a [u8]),
    Unescaped(String),
}

impl<'a> Key<'a> {
    fn as_bytes(&self) -> &[u8] {
        match self {
            Key::Plain(bs) => bs,
            Key::Unescaped(s) => s.as_bytes(),
        }
    }
    fn into_owned(self) -> String {
        match self {
            Key::Plain(bs) => String::from_utf8_lossy(bs).into_owned(),
            Key::Unescaped(s) => s,
        }
    }
}

/// Calls `f` for each top-level key/value of the object in `bs`. Stops at the first syntax error.
/// Returns whether `bs` is one complete object (nested values are only checked for balance).
fn parse_object<'a>(bs: &'a [u8], mut f: impl FnMut(Key<'a>, Val)) -> bool {
    let mut i = skip_ws(bs, 0);
    if bs.get(i) != Some(&b'{') {
        return false;
    }
    i = skip_ws(bs, i + 1);
    if bs.get(i) == Some(&b'}') {
        return skip_ws(bs, i + 1) == bs.len();
    }
    loop {
        i = skip_ws(bs, i);
        if bs.get(i) != Some(&b'"') {
            return false;
        }
        let Some((key_end, escaped)) = scan_string(bs, i) else { return false };
        let key = if escaped {
            match serde_json::from_slice::<String>(&bs[i..key_end]) {
                Ok(s) => Key::Unescaped(s),
                Err(_) => return false,
            }
        } else {
            Key::Plain(&bs[i + 1..key_end - 1])
        };
        i = skip_ws(bs, key_end);
        if bs.get(i) != Some(&b':') {
            return false;
        }
        i = skip_ws(bs, i + 1);
        let Some((val, next)) = scan_value(bs, i) else { return false };
        f(key, val);
        i = skip_ws(bs, next);
        match bs.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => return skip_ws(bs, i + 1) == bs.len(),
            _ => return false,
        }
    }
}

/// The top-level keys and values of a JSON object, with values as awk sees them (for
/// `zawk inspect`). None if `bs` is not an object.
pub fn object_cells(bs: &[u8]) -> Option<Vec<(String, crate::inspect::Cell)>> {
    use crate::inspect::Cell;
    if bs.get(skip_ws(bs, 0)) != Some(&b'{') {
        return None;
    }
    let mut out = Vec::new();
    parse_object(bs, |key, val| {
        let cell = match val {
            Val::Str { start, end, escaped } => {
                let unescaped = if escaped { serde_json::from_slice::<String>(&bs[start..end]).ok() } else { None };
                Cell::Str(unescaped.unwrap_or_else(|| String::from_utf8_lossy(&bs[start + 1..end - 1]).into_owned()))
            }
            Val::Raw { start, end } => {
                let text = String::from_utf8_lossy(&bs[start..end]).into_owned();
                if matches!(bs[start], b'{' | b'[') { Cell::Nested(text) } else { Cell::Num(text) }
            }
            Val::Null => Cell::Null,
            Val::True => Cell::Bool(true),
            Val::False => Cell::Bool(false),
        };
        out.push((key.into_owned(), cell));
    });
    Some(out)
}

/// Calls `f` for each element of the array in `bs`. Stops at the first syntax error. Returns
/// whether `bs` is one complete array.
fn parse_array(bs: &[u8], mut f: impl FnMut(Val)) -> bool {
    let mut i = skip_ws(bs, 0);
    if bs.get(i) != Some(&b'[') {
        return false;
    }
    i = skip_ws(bs, i + 1);
    if bs.get(i) == Some(&b']') {
        return skip_ws(bs, i + 1) == bs.len();
    }
    loop {
        i = skip_ws(bs, i);
        let Some((val, next)) = scan_value(bs, i) else { return false };
        f(val);
        i = skip_ws(bs, next);
        match bs.get(i) {
            Some(b',') => i += 1,
            Some(b']') => return skip_ws(bs, i + 1) == bs.len(),
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(json: &'static str, extra: &[&str]) -> (Vec<String>, Vec<String>) {
        let mut r = JsonlReader {
            inner: ByteReader::new(
                std::iter::once((std::io::Cursor::new(json.as_bytes()), String::from("-"))),
                NO_FIELD_SEP,
                b'\n',
                1024,
                false,
                ExecutionStrategy::Serial,
                CancelSignal::default(),
            ),
            scratch: DefaultLine::default(),
            shared: Default::default(),
            schema: Schema::default(),
            reported: 0,
            extra_names: extra.iter().map(|s| s.to_string()).collect(),
            header: Header::Start,
            used_fields: FieldSet::all(),
            line_no: Some(0),
            scalar_text: false,
        };
        let first = Str::from(json.lines().next().unwrap()).unmoor();
        r.build_schema(&first);
        let mut line = JsonlLine::default();
        r.fill(Str::from(json.lines().last().unwrap()).unmoor(), &mut line);
        (
            r.schema.names.clone(),
            line.fields.iter().map(|s| s.to_string()).collect(),
        )
    }

    #[test]
    fn object_values() {
        let (names, vals) = fields(
            r#"{"id": 1, "name":"a\"b", "ok":true, "no":false, "nil":null, "o":{"x":[1,"}"]}, "big":12345678901234567890}"#,
            &[],
        );
        assert_eq!(names, vec!["id", "name", "ok", "no", "nil", "o", "big"]);
        assert_eq!(
            vals,
            vec!["1", "a\"b", "1", "0", "", r#"{"x":[1,"}"]}"#, "12345678901234567890"]
        );
    }

    #[test]
    fn schema_from_first_line_plus_extras_then_new_keys() {
        let (names, vals) = fields("{\"a\":1,\"b\":2}\n{\"c\":3,\"b\":4,\"z\":5}", &["c", "a"]);
        // `z` is first seen in the last record: it is appended.
        assert_eq!(names, vec!["a", "b", "c", "z"]);
        assert_eq!(vals, vec!["", "4", "3", "5"]);
    }

    #[test]
    fn arrays_and_invalid() {
        let (_, vals) = fields("{}\n[1, \"x\", null]", &[]);
        assert_eq!(vals, vec!["1", "x", ""]);
        let (_, vals) = fields("{}\nnot json", &[]);
        assert!(vals.is_empty());
    }
}
