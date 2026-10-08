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
//! Value mapping: strings are unescaped, numbers keep their source text, `true`/`false` become
//! `1`/`0`, `null` and missing keys are empty, and nested objects/arrays keep their raw JSON text.
//! A top-level array is split positionally into `$1..$n`; a line that is not valid JSON has NF=0
//! while `$0` still holds the raw text.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use crate::common::{CancelSignal, ExecutionStrategy, Result};
use crate::pushdown::FieldSet;
use crate::runtime::{
    str_impl::Str,
    Int, RegexCache,
};

use super::{
    batch::ByteReader,
    chunk::{ChunkProducer, OffsetChunk},
    normalize_join_indexes, DefaultLine, Line, LineReader, ReaderState,
};

// NUL never appears in valid JSON text, so the inner reader never splits a record into fields.
const NO_FIELD_SEP: u8 = 0;

type Inner = ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>>;

#[derive(Default)]
struct Schema {
    names: Vec<String>,
    index: HashMap<Vec<u8>, usize>,
}

impl Schema {
    fn push(&mut self, name: String) {
        if !self.index.contains_key(name.as_bytes()) {
            self.index.insert(name.as_bytes().to_vec(), self.names.len());
            self.names.push(name);
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
        if col < 0 {
            return err!("attempt to access field {}; field must be nonnegative", col);
        }
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
        if col < 0 {
            return err!("attempt to access field {}; field must be nonnegative", col);
        }
        if col == 0 {
            self.line = s.clone().unmoor();
            self.diverged = false;
            return Ok(());
        }
        let col = col as usize - 1;
        if col >= self.fields.len() {
            self.fields.resize_with(col + 1, Str::default);
        }
        self.fields[col] = s.clone().unmoor();
        self.diverged = true;
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
            return Ok(());
        }
        self.fields.resize_with(nf as usize, Str::default);
        self.diverged = true;
        Ok(())
    }
}

pub struct JsonlReader {
    inner: Inner,
    scratch: DefaultLine,
    // Shared with the worker readers of a parallel run, which may be created before the main
    // reader sees the first record.
    schema: Arc<OnceLock<Schema>>,
    extra_names: Vec<String>,
    header: Header,
    used_fields: FieldSet,
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
            schema: Default::default(),
            extra_names,
            header: Header::Start,
            used_fields: FieldSet::all(),
        }
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
        let _ = self.schema.set(schema);
    }

    fn schema(&self) -> &Schema {
        static EMPTY: OnceLock<Schema> = OnceLock::new();
        self.schema.get().unwrap_or_else(|| EMPTY.get_or_init(Schema::default))
    }

    fn header_line(&self, out: &mut JsonlLine) {
        let names = &self.schema().names;
        out.fields.clear();
        out.fields
            .extend(names.iter().map(|n| Str::from(n.clone()).unmoor()));
        out.line = Str::from(names.join(",")).unmoor();
        out.diverged = false;
    }

    fn fill(&self, line: Str<'static>, out: &mut JsonlLine) {
        out.diverged = false;
        out.fields.clear();
        let schema = self.schema();
        let used = &self.used_fields;
        line.with_bytes(|bs| {
            let start = skip_ws(bs, 0);
            match bs.get(start) {
                Some(b'{') => {
                    out.fields.resize_with(schema.names.len(), Str::default);
                    let fields = &mut out.fields;
                    parse_object(bs, |key, val| {
                        if let Some(&ix) = schema.index.get(key.as_bytes())
                            && used.get(ix + 1)
                        {
                            fields[ix] = val.to_str(&line, bs);
                        }
                    });
                }
                Some(b'[') => {
                    let fields = &mut out.fields;
                    parse_array(bs, |val| fields.push(val.to_str(&line, bs)));
                }
                Some(_) => {
                    // A bare scalar: accept only a complete JSON string, number or literal.
                    if let Some((val, end)) = scan_value(bs, start) {
                        let valid = skip_ws(bs, end) == bs.len()
                            && match val {
                                Val::Raw { start, end } => std::str::from_utf8(&bs[start..end])
                                    .is_ok_and(|s| s.parse::<f64>().is_ok()),
                                _ => true,
                            };
                        if valid {
                            out.fields.push(val.to_str(&line, bs));
                        }
                    }
                }
                None => {}
            }
        });
        out.line = line;
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
                let schema = self.schema.clone();
                let used_fields = self.used_fields.clone();
                Box::new(move || JsonlReader {
                    inner: factory(),
                    scratch: DefaultLine::default(),
                    schema,
                    extra_names: Vec::new(),
                    header: Header::Done,
                    used_fields,
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
                let changed = self.inner.read_line_reuse(pat, rc, &mut self.scratch)?;
                let first = trim_cr(self.scratch.line.clone());
                self.build_schema(&first);
                self.header_line(old);
                let pending = if self.inner.read_state() == ReaderState::Eof as i64 {
                    None
                } else {
                    Some(first)
                };
                self.header = Header::Emitted(pending);
                Ok(changed)
            }
            Header::Emitted(Some(first)) => {
                self.fill(first, old);
                Ok(false)
            }
            Header::Emitted(None) | Header::Done => {
                let changed = self.inner.read_line_reuse(pat, rc, &mut self.scratch)?;
                let line = trim_cr(self.scratch.line.clone());
                self.fill(line, old);
                Ok(changed)
            }
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
fn parse_object<'a>(bs: &'a [u8], mut f: impl FnMut(Key<'a>, Val)) {
    let mut i = skip_ws(bs, 0);
    if bs.get(i) != Some(&b'{') {
        return;
    }
    i += 1;
    loop {
        i = skip_ws(bs, i);
        if bs.get(i) != Some(&b'"') {
            return;
        }
        let Some((key_end, escaped)) = scan_string(bs, i) else { return };
        let key = if escaped {
            match serde_json::from_slice::<String>(&bs[i..key_end]) {
                Ok(s) => Key::Unescaped(s),
                Err(_) => return,
            }
        } else {
            Key::Plain(&bs[i + 1..key_end - 1])
        };
        i = skip_ws(bs, key_end);
        if bs.get(i) != Some(&b':') {
            return;
        }
        i = skip_ws(bs, i + 1);
        let Some((val, next)) = scan_value(bs, i) else { return };
        f(key, val);
        i = skip_ws(bs, next);
        match bs.get(i) {
            Some(b',') => i += 1,
            _ => return,
        }
    }
}

/// Calls `f` for each element of the array in `bs`. Stops at the first syntax error.
fn parse_array(bs: &[u8], mut f: impl FnMut(Val)) {
    let mut i = skip_ws(bs, 0);
    if bs.get(i) != Some(&b'[') {
        return;
    }
    i += 1;
    loop {
        i = skip_ws(bs, i);
        let Some((val, next)) = scan_value(bs, i) else { return };
        f(val);
        i = skip_ws(bs, next);
        match bs.get(i) {
            Some(b',') => i += 1,
            _ => return,
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
            schema: Default::default(),
            extra_names: extra.iter().map(|s| s.to_string()).collect(),
            header: Header::Start,
            used_fields: FieldSet::all(),
        };
        let first = Str::from(json.lines().next().unwrap()).unmoor();
        r.build_schema(&first);
        let mut line = JsonlLine::default();
        r.fill(Str::from(json.lines().last().unwrap()).unmoor(), &mut line);
        (
            r.schema().names.clone(),
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
    fn schema_from_first_line_plus_extras() {
        let (names, vals) = fields("{\"a\":1,\"b\":2}\n{\"c\":3,\"b\":4,\"z\":5}", &["c", "a"]);
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(vals, vec!["", "4", "3"]);
    }

    #[test]
    fn arrays_and_invalid() {
        let (_, vals) = fields("{}\n[1, \"x\", null]", &[]);
        assert_eq!(vals, vec!["1", "x", ""]);
        let (_, vals) = fields("{}\nnot json", &[]);
        assert!(vals.is_empty());
    }
}
