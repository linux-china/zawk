//! Grok (`--grok PATTERN`) input support.
//!
//! Each line is matched against a grok expression such as `%{IP:client} %{WORD:method}`; the
//! named (aliased) captures become the columns: `$1..$NF` hold them in the order they appear in
//! the pattern, `FI` maps each name to its column index, and `$0` is the whole line. A capture
//! that did not take part in the match is empty. Lines that do not match are skipped (and
//! counted as rejected records).
//!
//! The columns are known as soon as the pattern is compiled. To reuse the `-H`/`FI` machinery,
//! the reader first emits a virtual header record whose fields are the column names, as the
//! JSON Lines reader does; the first matching line is then emitted as ordinary data.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::common::{CancelSignal, ExecutionStrategy, Result};
use crate::pushdown::FieldSet;
use crate::runtime::{Int, RegexCache, str_impl::Str};

use super::{
    DefaultLine, LineReader, ReaderState,
    batch::ByteReader,
    chunk::{ChunkProducer, OffsetChunk},
    jsonl::JsonlLine,
};

// NUL is not a field separator of a log line, so the inner reader never splits a line.
const NO_FIELD_SEP: u8 = 0;

type Inner = ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>>;

/// A compiled `--grok` expression and its columns.
#[derive(Debug)]
pub struct GrokSchema {
    pattern: grok::Pattern,
    names: Vec<String>,
}

impl GrokSchema {
    /// Compiles `pattern` with the default grok pattern library and the custom definitions
    /// `defs` (one `NAME regex` per line, as in Logstash patterns files). Fails when the pattern
    /// or a definition is invalid, or when the pattern has no named captures.
    pub fn new(pattern: &str, defs: &str) -> std::result::Result<GrokSchema, String> {
        use crate::runtime::string_util::{grok_compile, grok_definitions};
        let compiled = grok_compile(pattern, defs)?;
        let custom = grok_definitions(defs)?;
        let names = column_order(pattern, &custom, &compiled);
        if names.is_empty() {
            return Err(
                "no named captures: name the parts to extract, as in %{IP:client}".to_string(),
            );
        }
        Ok(GrokSchema {
            pattern: compiled,
            names,
        })
    }

    /// The column names, in column order.
    pub fn names(&self) -> &[String] {
        &self.names
    }
}

/// The capture names of `compiled` in the order they appear in `pattern`, following the
/// patterns it refers to (`%{COMBINEDAPACHELOG}` has the captures of its definition). Names that
/// cannot be placed this way come last, in alphabetical order.
fn column_order(pattern: &str, custom: &[(&str, &str)], compiled: &grok::Pattern) -> Vec<String> {
    // Names the captures as grok does: an alias used again is `alias[1]`, `alias[2]`, ...
    fn walk(
        pattern: &str,
        custom: &[(&str, &str)],
        depth: usize,
        counts: &mut HashMap<String, usize>,
        out: &mut Vec<String>,
    ) {
        use grok::parser::{GrokComponent, grok_split};
        if depth > 64 {
            return;
        }
        for component in grok_split(pattern) {
            if let GrokComponent::GrokPattern {
                name,
                alias,
                definition,
                ..
            } = component
            {
                if !alias.is_empty() {
                    let count = counts.entry(alias.to_string()).or_insert(0);
                    out.push(if *count == 0 {
                        alias.to_string()
                    } else {
                        format!("{alias}[{count}]")
                    });
                    *count += 1;
                }
                // the captures inside the pattern come after its own
                let body = if !definition.is_empty() {
                    Some(definition)
                } else {
                    // a custom definition replaces a default one of the same name
                    custom
                        .iter()
                        .rev()
                        .chain(grok::patterns())
                        .find(|(n, _)| *n == name)
                        .map(|(_, def)| *def)
                };
                if let Some(body) = body {
                    walk(body, custom, depth + 1, counts, out);
                }
            }
        }
    }
    let captures: HashSet<&str> = compiled.capture_names().collect();
    let mut ordered = Vec::new();
    walk(pattern, custom, 0, &mut HashMap::new(), &mut ordered);
    let mut seen = HashSet::new();
    let mut names: Vec<String> = ordered
        .into_iter()
        .filter(|n| captures.contains(n.as_str()) && seen.insert(n.clone()))
        .collect();
    // capture_names is sorted
    for name in compiled.capture_names() {
        if seen.insert(name.to_string()) {
            names.push(name.to_string());
        }
    }
    names
}

enum Header {
    // Nothing has been read yet.
    Start,
    // The virtual header was returned; the first record (if any) is still to be returned.
    Emitted(Option<Str<'static>>),
    Done,
}

pub struct GrokReader {
    inner: Inner,
    scratch: DefaultLine,
    schema: Arc<GrokSchema>,
    // The number of columns already in FI.
    reported: usize,
    header: Header,
    used_fields: FieldSet,
}

impl GrokReader {
    pub fn new<I, S>(
        rs: I,
        schema: Arc<GrokSchema>,
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
        GrokReader {
            inner,
            scratch: DefaultLine::default(),
            reported: schema.names.len(),
            schema,
            header: Header::Start,
            used_fields: FieldSet::all(),
        }
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

    /// Matches `line` and splits it into `out`. Returns false if the line does not match.
    fn fill(&self, line: Str<'static>, out: &mut JsonlLine) -> bool {
        out.diverged = false;
        out.assigned = false;
        out.fields.clear();
        let schema = &self.schema;
        let used = &self.used_fields;
        let matched = line.with_bytes(|bs| {
            let text = String::from_utf8_lossy(bs);
            let Some(matches) = schema.pattern.match_against(&text) else {
                return false;
            };
            let borrowed = matches!(text, std::borrow::Cow::Borrowed(_));
            out.fields.extend(schema.names.iter().enumerate().map(|(ix, name)| {
                if !used.get(ix + 1) {
                    return Str::default();
                }
                match matches.get(name) {
                    // Valid UTF-8: the value is a slice of the line.
                    Some(v) if borrowed => {
                        let start = v.as_ptr() as usize - text.as_ptr() as usize;
                        line.slice(start, start + v.len())
                    }
                    Some(v) => Str::from(v.to_string()).unmoor(),
                    None => Str::default(),
                }
            }));
            true
        });
        out.line = line;
        matched
    }

    /// Reads the next line, or None at the end of the input. The flag is whether a new file was
    /// started on the way.
    fn read_raw(&mut self, pat: &Str, rc: &mut RegexCache) -> Result<(bool, Option<Str<'static>>)> {
        let changed = self.inner.read_line_reuse(pat, rc, &mut self.scratch)?;
        if self.inner.read_state() != ReaderState::OK as i64 {
            return Ok((changed, None));
        }
        Ok((changed, Some(trim_cr(self.scratch.line.clone()))))
    }

    /// Reads the next matching line into `out`, skipping the others. At the end of the input,
    /// `out` is left empty.
    fn next_record(&mut self, pat: &Str, rc: &mut RegexCache, out: &mut JsonlLine) -> Result<bool> {
        let mut changed = false;
        loop {
            let (file_changed, line) = self.read_raw(pat, rc)?;
            changed |= file_changed;
            let Some(line) = line else {
                out.fields.clear();
                out.line = Str::default();
                out.diverged = false;
                out.assigned = false;
                return Ok(changed);
            };
            if self.fill(line, out) {
                return Ok(changed);
            }
            crate::runtime::stats::rejected_record();
        }
    }
}

impl LineReader for GrokReader {
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
                Box::new(move || GrokReader {
                    inner: factory(),
                    scratch: DefaultLine::default(),
                    schema,
                    // A worker's FI may not hold the header yet: report every column.
                    reported: 0,
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
                // Hold the first matching line back, so that the header is only returned when
                // there is a record (as for JSON Lines).
                let changed = self.next_record(pat, rc, old)?;
                let first = (self.inner.read_state() == ReaderState::OK as i64)
                    .then(|| old.line.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_follow_the_pattern() {
        let s = GrokSchema::new("%{WORD:method} %{IP:client} %{NUMBER:bytes:int}", "").unwrap();
        assert_eq!(s.names(), ["method", "client", "bytes"]);
    }

    #[test]
    fn columns_of_referenced_patterns() {
        let s = GrokSchema::new("%{COMMONAPACHELOG}", "").unwrap();
        assert_eq!(s.names()[0], "clientip");
        assert!(s.names().contains(&"response".to_string()));
    }

    #[test]
    fn repeated_name_gets_an_index() {
        // as the grok() function names them
        let s = GrokSchema::new("%{WORD:a} %{WORD:b} %{WORD:a}", "").unwrap();
        assert_eq!(s.names(), ["a", "b", "a[1]"]);
    }

    #[test]
    fn aliased_pattern_keeps_its_captures() {
        let defs = "PAIR %{WORD:k}=%{WORD:v}";
        let s = GrokSchema::new("%{PAIR:pair} %{NUMBER:n}", defs).unwrap();
        assert_eq!(s.names(), ["pair", "k", "v", "n"]);
    }

    #[test]
    fn invalid_patterns() {
        assert!(GrokSchema::new("%{NOPE:x}", "").unwrap_err().contains("unknown pattern"));
        assert!(GrokSchema::new("%{WORD} %{IP}", "").unwrap_err().contains("no named captures"));
        assert!(GrokSchema::new("%{WORD:x}", "BAD").unwrap_err().contains("invalid definition"));
    }

    #[test]
    fn columns_of_custom_definitions() {
        let defs = "# comment\nREQ %{WORD:method} %{NOTSPACE:path}\n\nLINE %{IP:client} %{REQ}\n";
        let s = GrokSchema::new("%{LINE} %{NUMBER:status}", defs).unwrap();
        assert_eq!(s.names(), ["client", "method", "path", "status"]);
    }
}
