//! Markdown table (`-i markdown`) input support.
//!
//! The first table of a Markdown document is converted to CSV, which is then read like
//! `-i csv -H` input: `$1..$NF` are the cells of a row, and `FI` maps each column name to its
//! index. Text around the table (titles, descriptions, notes) and any later tables are ignored.
//!
//! A header cell of `name:TYPE` (e.g. `actor_id:DOUBLE`) names the column `name`: the text after
//! the first `:` is a data type annotation.
//!
//! The table ends at the first line without a `|` delimiter: GitHub flavored Markdown reads such
//! a line right below a table (e.g. a caption like `first 20 of 5000 rows`, or attributes like
//! `{total=30, rows=10}`) as a row, but it is almost always a note about the table.
//!
//! The document is parsed with `pulldown-cmark` (GitHub flavored tables), so standard input and
//! S3 objects (`s3://bucket/key`) are read into memory.

use std::fs;
use std::io::{self, Read};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

enum Source {
    File(String),
    Stdin,
}

/// The first table of a Markdown file (or of standard input) as CSV, with a header line.
pub struct MarkdownCsv {
    source: Source,
    buf: Option<Vec<u8>>,
    pos: usize,
}

impl MarkdownCsv {
    pub fn file(path: String) -> MarkdownCsv {
        MarkdownCsv::new(Source::File(path))
    }
    pub fn stdin() -> MarkdownCsv {
        MarkdownCsv::new(Source::Stdin)
    }
    fn new(source: Source) -> MarkdownCsv {
        MarkdownCsv {
            source,
            buf: None,
            pos: 0,
        }
    }

    fn name(&self) -> &str {
        match &self.source {
            Source::File(path) => path,
            Source::Stdin => "-",
        }
    }

    fn error(&self, e: impl std::fmt::Display) -> io::Error {
        io::Error::other(format!(
            "cannot read markdown file `{}': {}",
            self.name(),
            e
        ))
    }

    fn load(&self) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        match &self.source {
            Source::File(path) if crate::runtime::s3::is_s3_url(path) => {
                data = crate::runtime::s3::read_object_bytes(path)?.to_vec();
            }
            Source::File(path) if crate::runtime::http::is_http_url(path) => {
                data = crate::runtime::http::read_bytes(path)?.to_vec();
            }
            Source::File(path) => {
                data =
                    fs::read(path).map_err(|e| crate::common::InputOpenError::new_io(path, &e))?
            }
            Source::Stdin => {
                io::stdin()
                    .read_to_end(&mut data)
                    .map_err(|e| self.error(e))?;
            }
        }
        let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&data);
        let text = String::from_utf8_lossy(data);
        let csv = first_table_csv(&text);
        if csv.is_none() {
            crate::runtime::stdlib_warning(
                "markdown",
                format!("no table found in `{}'", self.name()),
            );
        }
        Ok(csv.unwrap_or_default().into_bytes())
    }
}

impl Read for MarkdownCsv {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.buf.is_none() {
            self.buf = Some(self.load()?);
        }
        let buf = self.buf.as_ref().unwrap();
        let n = out.len().min(buf.len() - self.pos);
        out[..n].copy_from_slice(&buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The rows (header first) of the first table in `text`, or `None` if it has no table.
fn first_table(text: &str) -> Option<Vec<Vec<String>>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut in_table = false;
    let mut cell: Option<String> = None;
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::Table(_)) => in_table = true,
            Event::End(TagEnd::Table) => return Some(rows),
            _ if !in_table => {}
            Event::Start(Tag::TableRow) if !has_delimiter(&text[range]) => return Some(rows),
            Event::Start(Tag::TableHead) | Event::Start(Tag::TableRow) => rows.push(Vec::new()),
            Event::Start(Tag::TableCell) => cell = Some(String::new()),
            Event::End(TagEnd::TableCell) => {
                if let (Some(row), Some(c)) = (rows.last_mut(), cell.take()) {
                    // pulldown-cmark already trims the cell, so spaces written as `&#32;` stay.
                    row.push(c);
                }
            }
            // `<br>` is the line break of a table cell (`-o markdown` writes newlines so).
            Event::InlineHtml(s) if is_br(&s) => {
                if let Some(c) = cell.as_mut() {
                    c.push('\n');
                }
            }
            Event::Text(s) | Event::Code(s) | Event::InlineHtml(s) | Event::Html(s) => {
                if let Some(c) = cell.as_mut() {
                    c.push_str(&s);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(c) = cell.as_mut() {
                    c.push(' ');
                }
            }
            _ => {}
        }
    }
    if in_table { Some(rows) } else { None }
}

/// Whether an inline HTML tag is a line break: `<br>`, `<br/>` or `<br />`, in any case.
fn is_br(tag: &str) -> bool {
    let inner = tag.strip_prefix('<').and_then(|t| t.strip_suffix('>'));
    let inner = inner.map(|t| t.trim_end_matches('/').trim_end());
    inner.is_some_and(|t| t.eq_ignore_ascii_case("br"))
}

/// Whether a table row's source has a `|` cell delimiter (not escaped as `\|`).
fn has_delimiter(row: &str) -> bool {
    let bytes = row.as_bytes();
    let mut escaped = false;
    for &b in bytes {
        match b {
            b'\\' => escaped = !escaped,
            b'|' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

/// Whether a header suffix after `:` is a column type annotation (`DOUBLE`, `VARCHAR`,
/// `DECIMAL(18,3)`, `INTEGER[]`, ...). Anything else is part of the column name, so a column
/// named `host:port` (as `-o markdown` writes it) keeps its name.
fn is_type_name(ty: &str) -> bool {
    let lower = ty.trim().to_ascii_lowercase();
    let base = lower.trim_end_matches("[]");
    let base = base.split_once('(').map_or(base, |(b, _)| b).trim_end();
    matches!(
        base,
        "auto" | "string" | "str" | "utf8" | "text" | "varchar" | "char" | "bpchar"
            | "boolean" | "bool" | "number" | "numeric" | "decimal"
            | "int" | "integer" | "tinyint" | "smallint" | "bigint" | "hugeint" | "long"
            | "int8" | "int16" | "int32" | "int64" | "int128"
            | "utinyint" | "usmallint" | "uinteger" | "ubigint" | "uhugeint"
            | "uint8" | "uint16" | "uint32" | "uint64"
            | "float" | "float32" | "float64" | "real" | "double"
            | "date" | "time" | "timestamp" | "datetime" | "timestamptz" | "interval"
            | "timestamp with time zone" | "time with time zone"
            | "json" | "uuid" | "bytes" | "binary" | "blob"
    )
}

/// The first table in `text` as CSV, with the column names (without type annotations) first.
fn first_table_csv(text: &str) -> Option<String> {
    let mut rows = first_table(text)?;
    if let Some(header) = rows.first_mut() {
        for name in header.iter_mut() {
            if let Some((n, ty)) = name.rsplit_once(':')
                && is_type_name(ty)
            {
                *name = n.trim().to_string();
            }
        }
    }
    let mut csv = String::new();
    for row in rows.iter() {
        for (i, value) in row.iter().enumerate() {
            if i > 0 {
                csv.push(',');
            }
            if value.contains([',', '"', '\r', '\n']) {
                csv.push('"');
                csv.push_str(&value.replace('"', "\"\""));
                csv.push('"');
            } else {
                csv.push_str(value);
            }
        }
        csv.push('\n');
    }
    Some(csv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_table_only() {
        let text = "# Actors\n\nSome notes.\n\n\
            | actor_id:DOUBLE | first_name:VARCHAR | note |\n\
            |-----------------|--------------------|------|\n\
            | 1.0             | PENELOPE           | a, \"b\" |\n\
            | 2.0             | `NICK`             | x \\| y |\n\
            \nMore text.\n\n| other |\n|---|\n| 3 |\n";
        assert_eq!(
            first_table_csv(text).unwrap(),
            "actor_id,first_name,note\n1.0,PENELOPE,\"a, \"\"b\"\"\"\n2.0,NICK,x | y\n"
        );
    }

    #[test]
    fn colon_in_column_name_is_kept() {
        let text = "| host:port | v:INTEGER | d:DECIMAL(18,3) | a:b:VARCHAR | t: |\n\
            |---|---|---|---|---|\n\
            | a:1 | 2 | 3.5 | x | y |\n";
        assert_eq!(
            first_table_csv(text).unwrap(),
            "host:port,v,d,a:b,t:\na:1,2,3.5,x,y\n"
        );
    }

    #[test]
    fn table_ends_at_line_without_delimiter() {
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\nfirst 20 and last 20 of 5000 rows, hash 7389c2f5\n| 3 | 4 |\n";
        assert_eq!(first_table_csv(text).unwrap(), "a,b\n1,2\n");
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\n{total=30, rows=10}\n";
        assert_eq!(first_table_csv(text).unwrap(), "a,b\n1,2\n");
        let text = "| a | b |\n|---|---|\n| 1 | 2 |\nx \\| y\n";
        assert_eq!(first_table_csv(text).unwrap(), "a,b\n1,2\n");
        let text = "a | b\n--|--\n1 | 2\n";
        assert_eq!(first_table_csv(text).unwrap(), "a,b\n1,2\n");
    }

    #[test]
    fn no_table() {
        assert_eq!(first_table_csv("# Title\n\njust text\n"), None);
    }

    #[test]
    fn cell_encodings_round_trip() {
        // `-o markdown` writes edge spaces as `&#32;`, newlines as `<br>`, tabs and CRs as
        // character references; a hand-aligned table is still trimmed.
        let text = "| a | b | c |\n| --- | --- | --- |\n\
            | &#32;&#32;pad&#32; | l1<br>l2<BR/>l3 | x&#9;y&#13;z \\\\n |\n\
            | 1.0          | &amp;lt;br&gt; | &lt;br> |\n";
        assert_eq!(
            first_table(text).unwrap()[1..],
            [
                vec!["  pad ", "l1\nl2\nl3", "x\ty\rz \\n"],
                vec!["1.0", "&lt;br>", "<br>"],
            ]
        );
    }

    #[test]
    fn escaped_inline_markup_round_trip() {
        // `-o markdown` backslash-escapes inline markup so it reads back literally.
        let text = "| a | b | c |\n| --- | --- | --- |\n\
            | 2\\*3\\*4 | \\_id\\_ \\~\\~s\\~\\~ | \\`code\\` \\[t\\](http://u) \\*em\\* |\n";
        assert_eq!(
            first_table(text).unwrap()[1..],
            [vec!["2*3*4", "_id_ ~~s~~", "`code` [t](http://u) *em*"]]
        );
    }
}
