//! Apache Parquet (`-i parquet`) input support.
//!
//! A Parquet file is converted to JSON Lines, one JSON object per row, which is then read like
//! `-i jsonl` input: `$1..$NF` are the top-level columns in schema order, `FI` maps each column
//! name to its index, and `$0` is the row as a JSON object. Going through the JSON Lines reader
//! (rather than a reader type of its own) keeps the binary small, as the interpreter and the
//! compiled runtime are generic over the reader type.
//!
//! Rows are read with the record API of the `parquet` crate (without Arrow, also to keep the
//! binary small). Value mapping, following `-i jsonl`: numbers (decimals exactly), strings,
//! dates, times and timestamps (UTC) are plain text, booleans are `1`/`0`, null is empty, binary
//! values are hex, and nested values (LIST, MAP, STRUCT) and JSON columns are JSON text.
//!
//! Parquet keeps its metadata at the end of the file, so standard input is read into memory.

use std::fs::File;
use std::io::{self, Read};

use bytes::Bytes;
use ::parquet::basic::{ConvertedType, LogicalType, TimeUnit, Type as PhysicalType};
use ::parquet::data_type::Decimal;
use ::parquet::file::reader::{FileReader, SerializedFileReader};
use ::parquet::record::reader::RowIter;
use ::parquet::record::Field;
use ::parquet::schema::types::{Type, TypePtr};

/// How a value is written in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Null,
    Bool,
    /// The text is valid JSON as is: numbers and nested values.
    Raw,
    Str,
}

/// Special formatting of a top-level column, from its logical type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Plain,
    TimestampNanos,
    TimeNanos,
    Uuid,
    Json,
}

enum Source {
    File(String),
    Stdin,
}

/// The rows of a Parquet file (or of standard input) as JSON Lines.
pub struct ParquetJson {
    source: Source,
    rows: Option<Rows>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
}

struct Rows {
    iter: RowIter<'static>,
    columns: Vec<TypePtr>,
    formats: Vec<Format>,
    // Columns whose type the record API cannot read (written as null).
    unsupported: Vec<bool>,
}

impl ParquetJson {
    pub fn file(path: String) -> ParquetJson {
        ParquetJson::new(Source::File(path))
    }
    pub fn stdin() -> ParquetJson {
        ParquetJson::new(Source::Stdin)
    }
    fn new(source: Source) -> ParquetJson {
        ParquetJson { source, rows: None, buf: Vec::new(), pos: 0, done: false }
    }

    /// Check up front that `path` is a Parquet file that can be read, so that problems are
    /// reported clearly before any input is processed (rather than as a read error).
    pub fn check(path: &str) -> std::result::Result<(), String> {
        use ::parquet::basic::Compression;
        let error = |e: &dyn std::fmt::Display| format!("cannot read parquet file `{}': {}", path, e);
        let file = File::open(path).map_err(|e| error(&e))?;
        let reader = SerializedFileReader::new(file).map_err(|e| error(&e))?;
        for row_group in reader.metadata().row_groups() {
            for column in row_group.columns() {
                match column.compression() {
                    Compression::UNCOMPRESSED
                    | Compression::SNAPPY
                    | Compression::ZSTD(_)
                    | Compression::LZ4
                    | Compression::LZ4_RAW => {}
                    other => {
                        let name = match other {
                            Compression::GZIP(_) => "GZIP".to_string(),
                            Compression::BROTLI(_) => "BROTLI".to_string(),
                            other => other.to_string(),
                        };
                        return Err(error(&format!(
                            "unsupported compression {} (supported: SNAPPY, ZSTD, LZ4)",
                            name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        match &self.source {
            Source::File(path) => path,
            Source::Stdin => "-",
        }
    }

    fn error(&self, e: impl std::fmt::Display) -> io::Error {
        io::Error::other(format!("cannot read parquet file `{}': {}", self.name(), e))
    }

    fn open(&self) -> io::Result<Rows> {
        let reader: Box<dyn FileReader> = match &self.source {
            Source::File(path) => {
                let file = File::open(path).map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(file).map_err(|e| self.error(e))?)
            }
            Source::Stdin => {
                let mut data = Vec::new();
                io::stdin().read_to_end(&mut data).map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(Bytes::from(data)).map_err(|e| self.error(e))?)
            }
        };
        let root = reader.metadata().file_metadata().schema_descr_ptr();
        let columns: Vec<TypePtr> = root.root_schema().get_fields().to_vec();
        let formats = columns.iter().map(|c| column_format(c)).collect();
        let unsupported: Vec<bool> = columns.iter().map(|c| !supported(c)).collect();
        for (column, bad) in columns.iter().zip(&unsupported) {
            if *bad {
                crate::runtime::stdlib_warning(
                    "parquet",
                    format!("column `{}' of `{}' has an unsupported type, it is read as empty", column.name(), self.name()),
                );
            }
        }
        // Skip the unsupported columns: the record API panics on them.
        let projection = if unsupported.iter().any(|bad| *bad) {
            let fields: Vec<TypePtr> = columns
                .iter()
                .zip(&unsupported)
                .filter(|(_, bad)| !**bad)
                .map(|(c, _)| c.clone())
                .collect();
            let projection = Type::group_type_builder(root.root_schema().name())
                .with_fields(fields)
                .build()
                .map_err(|e| self.error(e))?;
            Some(projection)
        } else {
            None
        };
        let iter = RowIter::from_file_into(reader).project(projection).map_err(|e| self.error(e))?;
        Ok(Rows { iter, columns, formats, unsupported })
    }

    /// Append the next rows (about 64KB) to the buffer as JSON Lines.
    fn fill(&mut self) -> io::Result<()> {
        if self.rows.is_none() {
            self.rows = Some(self.open()?);
        }
        self.buf.clear();
        self.pos = 0;
        let mut line = String::new();
        while self.buf.len() < 64 << 10 {
            let rows = self.rows.as_mut().unwrap();
            let row = match rows.iter.next() {
                Some(Ok(row)) => row,
                Some(Err(e)) => return Err(self.error(e)),
                None => {
                    self.done = true;
                    break;
                }
            };
            line.clear();
            line.push('{');
            let mut values = row.get_column_iter();
            for (i, column) in rows.columns.iter().enumerate() {
                if i > 0 {
                    line.push(',');
                }
                push_json_str(&mut line, column.name());
                line.push(':');
                if rows.unsupported[i] {
                    line.push_str("null");
                    continue;
                }
                match values.next() {
                    Some((_, field)) => {
                        let (text, kind) = field_text(field, rows.formats[i]);
                        push_json(&mut line, &text, kind);
                    }
                    None => line.push_str("null"),
                }
            }
            line.push_str("}\n");
            self.buf.extend_from_slice(line.as_bytes());
        }
        Ok(())
    }
}

impl Read for ParquetJson {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            if self.done {
                return Ok(0);
            }
            self.fill()?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn push_json(out: &mut String, text: &str, kind: Kind) {
    match kind {
        Kind::Null => out.push_str("null"),
        Kind::Bool => out.push_str(if text == "1" { "true" } else { "false" }),
        Kind::Raw => out.push_str(text),
        Kind::Str => push_json_str(out, text),
    }
}

/// A JSON string literal.
fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Whether the record API can read every value of this column (it panics on some types, such
/// as INTERVAL).
fn supported(t: &Type) -> bool {
    if t.is_group() {
        return t.get_fields().iter().all(|f| supported(f));
    }
    use ConvertedType as C;
    let converted = t.get_basic_info().converted_type();
    match t.get_physical_type() {
        PhysicalType::BOOLEAN | PhysicalType::INT96 | PhysicalType::FLOAT | PhysicalType::DOUBLE => true,
        PhysicalType::INT32 => matches!(
            converted,
            C::NONE | C::INT_8 | C::INT_16 | C::INT_32 | C::UINT_8 | C::UINT_16 | C::UINT_32
                | C::DATE | C::TIME_MILLIS | C::DECIMAL
        ),
        PhysicalType::INT64 => matches!(
            converted,
            C::NONE | C::INT_64 | C::UINT_64 | C::TIME_MICROS | C::TIMESTAMP_MILLIS
                | C::TIMESTAMP_MICROS | C::DECIMAL
        ),
        PhysicalType::BYTE_ARRAY => matches!(
            converted,
            C::NONE | C::UTF8 | C::ENUM | C::JSON | C::BSON | C::DECIMAL
        ),
        PhysicalType::FIXED_LEN_BYTE_ARRAY => matches!(converted, C::NONE | C::DECIMAL),
    }
}

fn column_format(t: &Type) -> Format {
    if t.is_group() {
        return Format::Plain;
    }
    if t.get_basic_info().converted_type() == ConvertedType::JSON {
        return Format::Json;
    }
    match t.get_basic_info().logical_type_ref() {
        Some(LogicalType::Timestamp(ts)) if ts.unit == TimeUnit::NANOS => Format::TimestampNanos,
        Some(LogicalType::Time(time)) if time.unit == TimeUnit::NANOS => Format::TimeNanos,
        Some(LogicalType::Uuid) => Format::Uuid,
        Some(LogicalType::Json) => Format::Json,
        _ => Format::Plain,
    }
}

/// The awk text of a top-level value, and how it is written in the JSON of `$0`.
fn field_text(field: &Field, format: Format) -> (String, Kind) {
    match (field, format) {
        (Field::Long(nanos), Format::TimestampNanos) => (timestamp_nanos(*nanos), Kind::Str),
        (Field::Long(nanos), Format::TimeNanos) => (time_nanos(*nanos), Kind::Str),
        (Field::Bytes(b), Format::Uuid) if b.data().len() == 16 => (uuid(b.data()), Kind::Str),
        // JSON columns keep their text; it is embedded as is in $0 when it is valid JSON.
        (Field::Str(s), Format::Json) => {
            let valid = serde_json::from_str::<serde::de::IgnoredAny>(s).is_ok();
            (s.clone(), if valid { Kind::Raw } else { Kind::Str })
        }
        (Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_), _) => {
            let mut json = String::new();
            write_json(&mut json, field);
            (json, Kind::Raw)
        }
        _ => scalar_text(field),
    }
}

/// The awk text of a scalar value, and how it is written in JSON.
fn scalar_text(field: &Field) -> (String, Kind) {
    let int = |v: i128| (v.to_string(), Kind::Raw);
    match field {
        Field::Null => (String::new(), Kind::Null),
        Field::Bool(b) => ((if *b { "1" } else { "0" }).to_string(), Kind::Bool),
        Field::Byte(v) => int(*v as i128),
        Field::Short(v) => int(*v as i128),
        Field::Int(v) => int(*v as i128),
        Field::Long(v) => int(*v as i128),
        Field::UByte(v) => int(*v as i128),
        Field::UShort(v) => int(*v as i128),
        Field::UInt(v) => int(*v as i128),
        Field::ULong(v) => int(*v as i128),
        Field::Float16(v) => float_text(v.to_f32() as f64, Some(v.to_f32())),
        Field::Float(v) => float_text(*v as f64, Some(*v)),
        Field::Double(v) => float_text(*v, None),
        Field::Decimal(d) => match decimal_text(d) {
            Some(text) => (text, Kind::Raw),
            None => (hex(d.data()), Kind::Str),
        },
        Field::Str(s) => (s.clone(), Kind::Str),
        Field::Bytes(b) => (hex(b.data()), Kind::Str),
        Field::Date(days) => (date(*days), Kind::Str),
        Field::TimeMillis(ms) => (time_nanos(*ms as i64 * 1_000_000), Kind::Str),
        Field::TimeMicros(us) => (time_nanos(us * 1_000), Kind::Str),
        Field::TimestampMillis(ms) => (timestamp_nanos(ms.saturating_mul(1_000_000)), Kind::Str),
        Field::TimestampMicros(us) => (timestamp_nanos(us.saturating_mul(1_000)), Kind::Str),
        Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
            let mut json = String::new();
            write_json(&mut json, field);
            (json, Kind::Raw)
        }
    }
}

/// A float in its shortest form, without a trailing ".0" (`3.5`, `100`, `1e300`). Infinities
/// and NaN are not JSON numbers: they are written as null in JSON.
fn float_text(v: f64, single: Option<f32>) -> (String, Kind) {
    if !v.is_finite() {
        let text = if v.is_nan() { "nan" } else if v > 0.0 { "inf" } else { "-inf" };
        return (text.to_string(), Kind::Null);
    }
    let mut buf = ryu::Buffer::new();
    // the shortest text of a FLOAT is that of the f32 (1.1, not 1.100000023841858)
    let text = match single {
        Some(f) => buf.format_finite(f),
        None => buf.format_finite(v),
    };
    (text.strip_suffix(".0").unwrap_or(text).to_string(), Kind::Raw)
}

/// Write a value as JSON (nested values: LIST as an array, MAP and STRUCT as an object).
fn write_json(out: &mut String, field: &Field) {
    match field {
        Field::Group(row) => {
            out.push('{');
            for (i, (name, value)) in row.get_column_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_str(out, name);
                out.push(':');
                write_json(out, value);
            }
            out.push('}');
        }
        Field::ListInternal(list) => {
            out.push('[');
            for (i, value) in list.elements().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, value);
            }
            out.push(']');
        }
        Field::MapInternal(map) => {
            out.push('{');
            for (i, (key, value)) in map.entries().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // object keys are strings
                let key = match key {
                    Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
                        let mut json = String::new();
                        write_json(&mut json, key);
                        json
                    }
                    _ => scalar_text(key).0,
                };
                push_json_str(out, &key);
                out.push(':');
                write_json(out, value);
            }
            out.push('}');
        }
        _ => {
            let (text, kind) = scalar_text(field);
            push_json(out, &text, kind);
        }
    }
}

/// Exact text of a decimal, e.g. `12345.67` or `-0.50` (None if it does not fit in 128 bits).
fn decimal_text(d: &Decimal) -> Option<String> {
    let bytes = d.data();
    if bytes.is_empty() || bytes.len() > 16 {
        return None;
    }
    // big-endian two's complement
    let mut value: i128 = if bytes[0] & 0x80 != 0 { -1 } else { 0 };
    for b in bytes {
        value = (value << 8) | *b as i128;
    }
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    let scale = d.scale().max(0) as usize;
    if scale == 0 {
        return Some(format!("{sign}{digits}"));
    }
    let digits = format!("{:0>width$}", digits, width = scale + 1);
    let (int, frac) = digits.split_at(digits.len() - scale);
    Some(format!("{sign}{int}.{frac}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn uuid(b: &[u8]) -> String {
    let h = hex(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

fn date(days: i32) -> String {
    chrono::DateTime::from_timestamp(days as i64 * 86_400, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `2024-01-15 10:30:00`, with the fraction of a second only when it is not zero.
fn timestamp_nanos(nanos: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(nanos).naive_utc().to_string()
}

fn time_nanos(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let hms = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if frac == 0 {
        hms
    } else {
        let frac = format!("{:09}", frac);
        format!("{}.{}", hms, frac.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::parquet::data_type::ByteArray;

    #[test]
    fn decimals_are_exact() {
        assert_eq!(decimal_text(&Decimal::from_i32(1234567, 9, 2)).unwrap(), "12345.67");
        assert_eq!(decimal_text(&Decimal::from_i64(-50, 18, 2)).unwrap(), "-0.50");
        assert_eq!(decimal_text(&Decimal::from_i32(5, 9, 0)).unwrap(), "5");
        assert_eq!(decimal_text(&Decimal::from_i32(-7, 9, 3)).unwrap(), "-0.007");
        let big = Decimal::from_bytes(ByteArray::from(i128::MAX.to_be_bytes().to_vec()), 38, 10);
        assert_eq!(decimal_text(&big).unwrap(), "17014118346046923173168730371.5884105727");
    }

    #[test]
    fn scalars() {
        assert_eq!(float_text(3.5, None), ("3.5".to_string(), Kind::Raw));
        assert_eq!(float_text(100.0, None), ("100".to_string(), Kind::Raw));
        assert_eq!(float_text(1.1f32 as f64, Some(1.1f32)), ("1.1".to_string(), Kind::Raw));
        assert_eq!(float_text(f64::NAN, None).1, Kind::Null);
        assert_eq!(time_nanos(49_530_500_000_000), "13:45:30.5");
        assert_eq!(time_nanos(3_600_000_000_000), "01:00:00");
        assert_eq!(timestamp_nanos(1_705_314_600_123_456_789), "2024-01-15 10:30:00.123456789");
        assert_eq!(scalar_text(&Field::Bool(true)), ("1".to_string(), Kind::Bool));
        assert_eq!(scalar_text(&Field::Null), (String::new(), Kind::Null));
    }

    #[test]
    fn json_strings() {
        let mut out = String::new();
        push_json_str(&mut out, "a\"b\\c\nd\te\u{1}张");
        assert_eq!(out, r#""a\"b\\c\nd\te\u0001张""#);
        assert!(serde_json::from_str::<String>(&out).is_ok());
    }
}
