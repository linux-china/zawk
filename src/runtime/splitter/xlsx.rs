//! Excel workbook (`-i xlsx`) input support, with the `calamine` crate.
//!
//! A sheet is converted to CSV, which is then read like `-i csv` input: `$1..$NF` are the cells
//! of a row, from column A. With `-H`, the first row is the header and `FI` maps each column name
//! to its index; without it every row is a record.
//!
//! `book.xlsx#Sheet2` reads the sheet `Sheet2` (the name is matched ignoring case when no sheet
//! has exactly that name); without `#`, the first sheet is read. A file whose name contains `#`
//! is read as a whole when it exists. Rows start at the first non-empty row of the sheet.
//!
//! Values are plain text: numbers as awk prints them (`48`, `1.5`), booleans `1`/`0`, dates as
//! `2024-01-05` or `2024-01-05 08:30:00`, times as `08:30:00`, errors as their text (`#DIV/0!`)
//! and empty cells as the empty string. `.xls`, `.xlsm`, `.xlsb` and `.ods` files are read too.
//!
//! The workbook is read into memory (also from standard input, URLs and S3 objects).

use std::io::{self, Cursor, Read};

use calamine::{open_workbook_auto_from_rs, Data, Reader};

use crate::runtime::numfmt::float_to_text;

/// The extensions of the workbooks that are read as `-i xlsx` by default.
const EXTENSIONS: &[&str] = &[".xlsx", ".xlsm", ".xls", ".xlsb", ".ods"];

/// `book.xlsx#Sheet2` → (`book.xlsx`, `Some("Sheet2")`). A path that exists as a file is not
/// split, so file names may contain `#`.
pub fn split_sheet(path: &str) -> (&str, Option<&str>) {
    match path.rsplit_once('#') {
        Some((file, sheet)) if !std::path::Path::new(path).exists() => (file, Some(sheet)),
        _ => (path, None),
    }
}

/// Whether `path` (without its `#sheet`) has the extension of a workbook.
pub fn is_xlsx_file(path: &str) -> bool {
    let file = split_sheet(path).0.to_ascii_lowercase();
    EXTENSIONS.iter().any(|ext| file.ends_with(ext))
}

/// A sheet of a workbook: its rows, each with the cells from column A.
pub struct Sheet {
    pub rows: Vec<Vec<Data>>,
}

impl Sheet {
    /// Reads the sheet of `path` (`book.xlsx#Sheet2`, or the first sheet), from a file, a URL, an
    /// S3 object or standard input (`-`).
    pub fn open(path: &str) -> io::Result<Sheet> {
        let (file, sheet) = split_sheet(path);
        // reported as an input that cannot be opened
        let error = |e: String| {
            io::Error::other(crate::common::InputOpenError(format!("cannot read xlsx file `{}': {}", file, e)))
        };
        let data = load(file)?;
        let mut book = open_workbook_auto_from_rs(Cursor::new(data)).map_err(|e| error(e.to_string()))?;
        let names = book.sheet_names();
        let name = match sheet {
            None => names.first().cloned().ok_or_else(|| error("the workbook has no sheet".to_string()))?,
            Some(sheet) => names
                .iter()
                .find(|n| n.as_str() == sheet)
                .or_else(|| names.iter().find(|n| n.eq_ignore_ascii_case(sheet)))
                .cloned()
                .ok_or_else(|| error(format!("no sheet `{}' (the sheets are {})", sheet, names.join(", "))))?,
        };
        let range = book.worksheet_range(&name).map_err(|e| error(e.to_string()))?;
        // The range starts at its first non-empty cell: the cells before it are empty, so that
        // `$1` is always column A.
        let skip = range.start().map_or(0, |(_, col)| col as usize);
        let rows = range
            .rows()
            .map(|row| std::iter::repeat_n(Data::Empty, skip).chain(row.iter().cloned()).collect())
            .collect();
        Ok(Sheet { rows })
    }
}

fn load(file: &str) -> io::Result<Vec<u8>> {
    if file == "-" {
        let mut data = Vec::new();
        io::stdin().read_to_end(&mut data)?;
        Ok(data)
    } else if crate::runtime::s3::is_s3_url(file) {
        Ok(crate::runtime::s3::read_object_bytes(file)?.to_vec())
    } else if crate::runtime::http::is_http_url(file) {
        Ok(crate::runtime::http::read_bytes(file)?.to_vec())
    } else {
        std::fs::read(file).map_err(|e| crate::common::InputOpenError::new_io(file, &e))
    }
}

/// The text of a cell as awk sees it.
pub fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::Int(n) => n.to_string(),
        Data::Float(f) => float_to_text(*f),
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        Data::Error(e) => e.to_string(),
        Data::DateTime(dt) => {
            let (y, mo, d, h, mi, s, ms) = dt.to_ymd_hms_milli();
            let time = if ms > 0 { format!("{:02}:{:02}:{:02}.{:03}", h, mi, s, ms) } else { format!("{:02}:{:02}:{:02}", h, mi, s) };
            if dt.is_duration() {
                // whole hours, as `[h]:mm:ss` in Excel, with the milliseconds as for a time
                let millis = (dt.as_f64() * 86_400_000.0).round() as i64;
                let sign = if millis < 0 { "-" } else { "" };
                let (secs, ms) = (millis.unsigned_abs() / 1000, millis.unsigned_abs() % 1000);
                let hms = format!("{}{:02}:{:02}:{:02}", sign, secs / 3600, secs / 60 % 60, secs % 60);
                if ms > 0 { format!("{}.{:03}", hms, ms) } else { hms }
            } else if dt.as_f64() < 1.0 {
                time
            } else if (h, mi, s, ms) == (0, 0, 0, 0) {
                format!("{:04}-{:02}-{:02}", y, mo, d)
            } else {
                format!("{:04}-{:02}-{:02} {}", y, mo, d, time)
            }
        }
    }
}

/// A sheet of a workbook (or of standard input) as CSV, or as TSV for `-i tsv`. It is read on the
/// first read.
pub struct XlsxCsv {
    path: String,
    tsv: bool,
    buf: Option<Vec<u8>>,
    pos: usize,
}

impl XlsxCsv {
    pub fn new(path: String) -> XlsxCsv {
        XlsxCsv { path, tsv: false, buf: None, pos: 0 }
    }

    /// The sheet as TSV, each cell escaped as `-o tsv` does.
    pub fn tsv(path: String) -> XlsxCsv {
        XlsxCsv { path, tsv: true, buf: None, pos: 0 }
    }

    fn load(&self) -> io::Result<Vec<u8>> {
        let sheet = Sheet::open(&self.path)?;
        let mut out = Vec::new();
        for row in &sheet.rows {
            let cells: Vec<String> = row.iter().map(cell_text).collect();
            if self.tsv {
                let cells: Vec<String> = cells.iter().map(|c| crate::runtime::str_escape::escape_tsv(c)).collect();
                out.extend_from_slice(cells.join("\t").as_bytes());
            } else {
                let cells: Vec<&str> = cells.iter().map(String::as_str).collect();
                out.extend_from_slice(crate::runtime::csv::vec_to_csv(&cells).as_bytes());
            }
            out.push(b'\n');
        }
        Ok(out)
    }
}

impl Read for XlsxCsv {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn demo(sheet: &str) -> String {
        let path = format!("{}/tests/demo.xlsx{}", env!("CARGO_MANIFEST_DIR"), sheet);
        let mut text = String::new();
        XlsxCsv::new(path).read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn test_split_sheet() {
        assert_eq!(split_sheet("no/such/book.xlsx#Sheet2"), ("no/such/book.xlsx", Some("Sheet2")));
        assert_eq!(split_sheet("book.xlsx"), ("book.xlsx", None));
        assert!(is_xlsx_file("a/b.XLSX#s1"));
        assert!(is_xlsx_file("b.ods"));
        assert!(!is_xlsx_file("b.csv"));
    }

    #[test]
    fn test_demo_sheets() {
        assert_eq!(demo(""), "name,gender,age\nJackie,M,48\nLove,F,49\n");
        assert_eq!(demo("#Sheet1"), demo(""));
        // the name is matched ignoring case
        assert_eq!(demo("#sheet2"), demo("#Sheet2"));
        assert!(demo("#Sheet2").starts_with("id,nick,status\n"));
        let err = Sheet::open(&format!("{}/tests/demo.xlsx#nosuch", env!("CARGO_MANIFEST_DIR"))).err().unwrap();
        assert!(err.to_string().contains("no sheet `nosuch' (the sheets are Sheet1, Sheet2)"), "{}", err);
    }

    #[test]
    fn test_demo_tsv() {
        let path = format!("{}/tests/demo.xlsx", env!("CARGO_MANIFEST_DIR"));
        let mut text = String::new();
        XlsxCsv::tsv(path).read_to_string(&mut text).unwrap();
        assert_eq!(text, "name\tgender\tage\nJackie\tM\t48\nLove\tF\t49\n");
    }

    #[test]
    fn test_cell_types() {
        let path = format!("{}/tests/types.xlsx", env!("CARGO_MANIFEST_DIR"));
        let mut text = String::new();
        XlsxCsv::new(path.clone()).read_to_string(&mut text).unwrap();
        assert_eq!(
            text,
            "name,born,start,seen,active,score,note\n\
             Jackie,1978-01-05,08:30:00,2024-01-05 13:45:10,1,1.5,\"a,b\"\n\
             Love,1977-12-31,23:59:59,2024-02-29,0,2,\n"
        );
        // data from B2: `$1` is column A, the empty rows above are skipped
        let mut text = String::new();
        XlsxCsv::new(format!("{}#offset", path)).read_to_string(&mut text).unwrap();
        assert_eq!(text, ",x,1\n,y,2\n");
    }

    #[test]
    fn test_duration_text() {
        use calamine::{ExcelDateTime, ExcelDateTimeType};
        let duration = |days: f64| cell_text(&Data::DateTime(ExcelDateTime::new(days, ExcelDateTimeType::TimeDelta, false)));
        let secs = |s: f64| duration(s / 86400.0);
        assert_eq!(secs(5405.0), "01:30:05");
        assert_eq!(secs(-5405.0), "-01:30:05");
        assert_eq!(secs(5405.25), "01:30:05.250");
        assert_eq!(secs(-0.5), "-00:00:00.500");
        assert_eq!(secs(0.0), "00:00:00");
        assert_eq!(duration(2.0), "48:00:00");
    }
}
