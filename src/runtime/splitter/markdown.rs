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
        MarkdownCsv { source, buf: None, pos: 0 }
    }

    fn name(&self) -> &str {
        match &self.source {
            Source::File(path) => path,
            Source::Stdin => "-",
        }
    }

    fn error(&self, e: impl std::fmt::Display) -> io::Error {
        io::Error::other(format!("cannot read markdown file `{}': {}", self.name(), e))
    }

    fn load(&self) -> io::Result<Vec<u8>> {
        let mut data = Vec::new();
        match &self.source {
            Source::File(path) if crate::runtime::s3::is_s3_url(path) => {
                data = crate::runtime::s3::read_object_bytes(path)?.to_vec();
            }
            Source::File(path) => data = fs::read(path).map_err(|e| self.error(e))?,
            Source::Stdin => {
                io::stdin().read_to_end(&mut data).map_err(|e| self.error(e))?;
            }
        }
        let text = String::from_utf8_lossy(&data);
        let csv = first_table_csv(&text);
        if csv.is_none() {
            crate::runtime::stdlib_warning("markdown", format!("no table found in `{}'", self.name()));
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
                    row.push(c.trim().to_string());
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

/// The first table in `text` as CSV, with the column names (without type annotations) first.
fn first_table_csv(text: &str) -> Option<String> {
    let mut rows = first_table(text)?;
    if let Some(header) = rows.first_mut() {
        for name in header.iter_mut() {
            if let Some((n, _)) = name.split_once(':') {
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
}
