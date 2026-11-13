//! Excel output (`-o xlsx`).
//!
//! Records printed to stdout are written as TSV (as for `-o tsv`) and converted to a workbook with
//! one sheet, `Sheet1`, written when stdout is closed, at the end of the program. The first record
//! is the header row (bold, frozen); a column name may end with `:string`, `:number`, `:boolean`
//! or `:auto` (the default), as for `-o jsonl`. Cell values:
//!
//! * `:auto`: a number as JSON writes it (`48`, `-1.5`, `2e3`; not `007`, `+1` or `0x10`) is a
//!   number cell, `true`/`false` (any case) a boolean cell, anything else a text cell. Integers
//!   with more than 15 digits (beyond the precision of Excel) stay text, so long IDs are kept.
//! * `:number`: a numeric value is a number cell; a value that is not a number is kept as text and
//!   reported.
//! * `:boolean`: true/false as for `-o parquet` (`yes`, `1`, ... are true).
//! * `:string`: always text.
//!
//! Empty values and the `--null` text are blank cells. Excel limits a sheet to 1,048,576 rows and
//! 16,384 columns and a cell to 32,767 characters: rows and columns beyond them are dropped and
//! longer text is truncated, with a warning on stderr. The workbook is built in memory.

use std::io::{self, Write};

use rust_xlsxwriter::{Format, Workbook, Worksheet, XlsxError};

use super::command::CommandEnv;
use super::writers::{FileFactory, JsonType, json_bool, json_column, json_number};
use crate::common::FileSpec;
use crate::runtime::str_escape::{is_tsv_null, unescape_tsv};

const MAX_ROWS: u32 = 1_048_576;
const MAX_COLS: usize = 16_384;
/// The maximum length of a cell's text, in UTF-16 code units.
const MAX_TEXT: usize = 32_767;
/// Integers with more digits lose precision as Excel numbers (IEEE doubles shown with 15 digits).
const MAX_DIGITS: usize = 15;

/// Wraps a factory so that stdout is written as an Excel workbook (`-o xlsx`).
#[derive(Clone)]
pub struct XlsxFactory<F>(pub F);

impl<F: FileFactory> FileFactory for XlsxFactory<F> {
    type Output = F::Output;
    type Stdout = XlsxWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<std::process::Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &std::path::Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        XlsxWriter::new(self.0.stdout())
    }
}

/// Converts TSV records written to it into an xlsx workbook written to `inner` when it is dropped;
/// see the module documentation.
pub struct XlsxWriter<W: Write> {
    inner: W,
    /// The current, not yet newline-terminated, record.
    pending: Vec<u8>,
    sheet: Worksheet,
    /// The column types, once the header has been read.
    types: Option<Vec<JsonType>>,
    /// The next row to write.
    row: u32,
    /// The number of columns that cells were written to.
    width: usize,
    dropped_rows: u64,
    dropped_cols: u64,
    truncated: u64,
    /// Per column, the `:number` values that are not numbers (kept as text).
    invalid: Vec<u64>,
    /// Set after the workbook was written (or failed to be).
    done: bool,
}

fn to_io(e: XlsxError) -> io::Error {
    io::Error::other(format!("xlsx output: {}", e))
}

/// `s` cut to at most `MAX_TEXT` UTF-16 code units, and whether it was cut.
fn fit_text(s: &str) -> (&str, bool) {
    // Every char is at most 2 UTF-16 code units and at least 1 byte.
    if s.len() <= MAX_TEXT {
        return (s, false);
    }
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > MAX_TEXT {
            return (&s[..i], true);
        }
    }
    (s, false)
}

/// The significant digits of a number's mantissa: `9999999999999999.0` has 16, `1e16` has 1.
fn significant_digits(s: &str) -> usize {
    let mantissa = s.split(['e', 'E']).next().unwrap_or(s);
    let digits = mantissa.trim_start_matches('-').replace('.', "");
    digits.trim_start_matches('0').trim_end_matches('0').len()
}

/// An `:auto` value as a number, unless it is an integer that does not fit Excel's precision.
///
/// Integers written with more than `MAX_DIGITS` digits stay text (likely IDs), and so do
/// integer values with more significant digits than Excel keeps, whatever the notation:
/// `9999999999999999.0` and `99999999999999999e1` would be silently rounded.
fn auto_number(s: &str) -> Option<f64> {
    let integer = json_number(s)?;
    if integer && s.trim_start_matches('-').len() > MAX_DIGITS {
        return None;
    }
    let f = s.parse::<f64>().ok().filter(|f| f.is_finite())?;
    if f.fract() == 0.0 && significant_digits(s) > MAX_DIGITS {
        return None;
    }
    Some(f)
}

impl<W: Write> XlsxWriter<W> {
    pub fn new(inner: W) -> XlsxWriter<W> {
        let mut sheet = Worksheet::new();
        let _ = sheet.set_name("Sheet1");
        XlsxWriter {
            inner,
            pending: Vec::new(),
            sheet,
            types: None,
            row: 0,
            width: 0,
            dropped_rows: 0,
            dropped_cols: 0,
            truncated: 0,
            invalid: Vec::new(),
            done: false,
        }
    }

    fn write_text(&mut self, col: u16, s: &str, format: Option<&Format>) -> Result<(), XlsxError> {
        let (text, cut) = fit_text(s);
        if cut {
            self.truncated += 1;
        }
        match format {
            Some(f) => self
                .sheet
                .write_string_with_format(self.row, col, text, f)?,
            None => self.sheet.write_string(self.row, col, text)?,
        };
        Ok(())
    }

    fn write_record(&mut self, line: &[u8]) -> Result<(), XlsxError> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if self.row >= MAX_ROWS {
            self.dropped_rows += 1;
            return Ok(());
        }
        let raw_cells: Vec<&[u8]> = line.split(|b| *b == b'\t').collect();
        if raw_cells.len() > MAX_COLS {
            self.dropped_cols += (raw_cells.len() - MAX_COLS) as u64;
        }
        let raw_cells = &raw_cells[..raw_cells.len().min(MAX_COLS)];
        self.width = self.width.max(raw_cells.len());
        if self.types.is_none() {
            let bold = Format::new().set_bold();
            let mut types = Vec::with_capacity(raw_cells.len());
            for (i, raw) in raw_cells.iter().enumerate() {
                let cell = unescape_tsv(&String::from_utf8_lossy(raw)).into_owned();
                let (name, ty) = json_column(&cell);
                types.push(ty.unwrap_or(JsonType::Auto));
                self.write_text(i as u16, name, Some(&bold))?;
            }
            self.types = Some(types);
            self.sheet.set_freeze_panes(1, 0)?;
            self.row += 1;
            return Ok(());
        }
        for (i, raw) in raw_cells.iter().enumerate() {
            if raw.is_empty() || is_tsv_null(raw) {
                continue;
            }
            let col = i as u16;
            let ty = self
                .types
                .as_ref()
                .and_then(|t| t.get(i).copied())
                .unwrap_or(JsonType::Auto);
            let text = String::from_utf8_lossy(raw);
            let cell = unescape_tsv(&text);
            match ty {
                JsonType::String => self.write_text(col, &cell, None)?,
                JsonType::Boolean => {
                    let b = crate::runtime::math_util::parse_bool(&cell);
                    self.sheet.write_boolean(self.row, col, b)?;
                }
                JsonType::Number => match cell.trim().parse::<f64>() {
                    Ok(f) if f.is_finite() => {
                        self.sheet.write_number(self.row, col, f)?;
                    }
                    _ => {
                        if self.invalid.len() <= i {
                            self.invalid.resize(i + 1, 0);
                        }
                        self.invalid[i] += 1;
                        self.write_text(col, &cell, None)?;
                    }
                },
                JsonType::Auto => {
                    if let Some(f) = auto_number(&cell) {
                        self.sheet.write_number(self.row, col, f)?;
                    } else if let Some(b) = json_bool(&cell) {
                        self.sheet.write_boolean(self.row, col, b)?;
                    } else {
                        self.write_text(col, &cell, None)?;
                    }
                }
            }
        }
        self.row += 1;
        Ok(())
    }

    fn warnings(&self) {
        if self.dropped_rows > 0 {
            eprintln!(
                "zawk: xlsx output: a sheet has at most {} rows: {} row(s) dropped",
                MAX_ROWS, self.dropped_rows
            );
        }
        if self.dropped_cols > 0 {
            eprintln!(
                "zawk: xlsx output: a sheet has at most {} columns: {} cell(s) dropped",
                MAX_COLS, self.dropped_cols
            );
        }
        if self.truncated > 0 {
            eprintln!(
                "zawk: xlsx output: a cell holds at most {} characters: {} value(s) truncated",
                MAX_TEXT, self.truncated
            );
        }
        for (i, n) in self.invalid.iter().enumerate() {
            if *n > 0 {
                eprintln!(
                    "zawk: xlsx output: column {} (:number): {} value(s) that are not numbers written as text",
                    i + 1,
                    n
                );
            }
        }
    }

    /// Writes the workbook.
    fn finish(&mut self) -> io::Result<()> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        // A final record without a trailing newline (e.g. from printf) is still a row.
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.write_record(&line).map_err(to_io)?;
        }
        self.warnings();
        if self.width > 0 {
            self.sheet.autofit();
        }
        let sheet = std::mem::replace(&mut self.sheet, Worksheet::new());
        let mut book = Workbook::new();
        book.push_worksheet(sheet);
        let data = book.save_to_buffer().map_err(to_io)?;
        self.inner.write_all(&data)?;
        self.inner.flush()
    }
}

impl<W: Write> Write for XlsxWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.done {
            return Ok(buf.len());
        }
        let mut rest = buf;
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            let r = if self.pending.is_empty() {
                self.write_record(&rest[..pos])
            } else {
                self.pending.extend_from_slice(&rest[..pos]);
                let line = std::mem::take(&mut self.pending);
                self.write_record(&line)
            };
            if let Err(e) = r {
                self.done = true;
                return Err(to_io(e));
            }
            rest = &rest[pos + 1..];
        }
        self.pending.extend_from_slice(rest);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // Nothing is written before the program ends.
        Ok(())
    }
}

impl<W: Write> Drop for XlsxWriter<W> {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            crate::runtime::writers::report_stdout_render_error(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calamine::{Data, Reader, Xlsx};

    fn cells(data: &[u8]) -> Vec<Vec<Data>> {
        let mut out = Vec::new();
        {
            let mut w = XlsxWriter::new(&mut out);
            w.write_all(data).unwrap();
        }
        let mut book: Xlsx<_> = Xlsx::new(io::Cursor::new(out)).unwrap();
        assert_eq!(book.sheet_names(), ["Sheet1"]);
        let range = book.worksheet_range("Sheet1").unwrap();
        range.rows().map(|r| r.to_vec()).collect()
    }

    fn s(v: &str) -> Data {
        Data::String(v.into())
    }

    #[test]
    fn auto_values() {
        let rows = cells(b"id\tname\tok\tcode\tbig\n1\tJackie\ttrue\t007\t12345678901234567\n-1.5\ta\\tb\tFALSE\t2e3\t");
        assert_eq!(rows[0], [s("id"), s("name"), s("ok"), s("code"), s("big")]);
        assert_eq!(
            rows[1],
            [
                Data::Float(1.0),
                s("Jackie"),
                Data::Bool(true),
                s("007"),
                s("12345678901234567")
            ]
        );
        // The last record has no newline; an empty value is a blank cell.
        assert_eq!(
            rows[2],
            [
                Data::Float(-1.5),
                s("a\tb"),
                Data::Bool(false),
                Data::Float(2000.0),
                Data::Empty
            ]
        );
    }

    #[test]
    fn typed_columns() {
        let rows = cells(
            b"zip:string\tn:number\tb:boolean\thost:port\n02134\t 7 \tyes\t1\n1\tx\t0\ta:b\n",
        );
        assert_eq!(rows[0], [s("zip"), s("n"), s("b"), s("host:port")]);
        assert_eq!(
            rows[1],
            [
                s("02134"),
                Data::Float(7.0),
                Data::Bool(true),
                Data::Float(1.0)
            ]
        );
        assert_eq!(rows[2], [s("1"), s("x"), Data::Bool(false), s("a:b")]);
    }

    #[test]
    fn number_bounds() {
        // Non-finite and overflowing values are text, in :number and :auto columns alike; the
        // 15-digit limit of :auto integers is exact, in any notation; scientific notation and -0
        // are numbers.
        let rows = cells(
            concat!(
                "n:number\ta\n",
                "1e309\t1e999\n",
                "inf\t-inf\n",
                "nan\tNaN\n",
                "-0\t-0\n",
                "1.7976931348623157e308\t-1.7976931348623157e308\n",
                "999999999999999\t999999999999999\n",
                "9999999999999999\t9999999999999999\n",
                "2.5e-3\t1E5\n",
                "9999999999999999.0\t9999999999999999.0\n",
                "1e16\t-1e16\n",
                "1000000000000000\t999999999999999.5\n",
                "1\t99999999999999999e1\n",
                "1\t10000000000000000.0\n",
                "\t\n",
            )
            .as_bytes(),
        );
        let f = Data::Float;
        assert_eq!(rows[1], [s("1e309"), s("1e999")]);
        assert_eq!(rows[2], [s("inf"), s("-inf")]);
        assert_eq!(rows[3], [s("nan"), s("NaN")]);
        assert_eq!(rows[4], [f(0.0), f(0.0)]);
        // 17 significant digits: Excel would round it (past f64::MAX), so :auto keeps the text.
        assert_eq!(rows[5], [f(f64::MAX), s("-1.7976931348623157e308")]);
        assert_eq!(rows[6], [f(999999999999999.0), f(999999999999999.0)]);
        assert_eq!(rows[7], [f(1e16), s("9999999999999999")]);
        assert_eq!(rows[8], [f(0.0025), f(100000.0)]);
        // The :auto limit is on significant digits, whatever the notation.
        assert_eq!(rows[9], [f(1e16), s("9999999999999999.0")]);
        assert_eq!(rows[10], [f(1e16), f(-1e16)]);
        assert_eq!(rows[11], [f(1e15), f(999999999999999.5)]);
        assert_eq!(rows[12], [f(1.0), s("99999999999999999e1")]);
        assert_eq!(rows[13], [f(1.0), f(1e16)]);
        assert_eq!(rows.len(), 14, "an empty row only has blank cells");
    }

    #[test]
    fn significant_digits_of_numbers() {
        assert_eq!(significant_digits("9999999999999999"), 16);
        assert_eq!(significant_digits("-9999999999999999.0"), 16);
        assert_eq!(significant_digits("1e16"), 1);
        assert_eq!(significant_digits("0.00120"), 2);
        assert_eq!(significant_digits("1.7976931348623157e308"), 17);
        assert_eq!(significant_digits("0"), 0);
    }

    #[test]
    fn empty_output() {
        assert!(cells(b"").is_empty());
    }

    #[test]
    fn long_text() {
        let long = "é".repeat(MAX_TEXT + 10);
        let (cut, was_cut) = fit_text(&long);
        assert!(was_cut);
        assert_eq!(cut.chars().count(), MAX_TEXT);
        let wide = "😀".repeat(MAX_TEXT);
        assert_eq!(fit_text(&wide).0.chars().count(), MAX_TEXT / 2);
        assert_eq!(fit_text("abc"), ("abc", false));
    }
}
