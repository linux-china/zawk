//! Apache Parquet output (`-o parquet`).
//!
//! Records printed to stdout are written as TSV (as for `-o tsv`) and converted to a Parquet file
//! by `ParquetWriter`. The first record is the header: each field is a column name, optionally
//! followed by `:TYPE` (case-insensitive, e.g. `age:INT32`), and columns without a type are
//! strings. The following records are data rows. Rows are buffered and written in row groups;
//! the file footer is written when stdout is closed, at the end of the program.
//!
//! All columns are OPTIONAL. Empty or unconvertible values are written as null, except for
//! `STRING` (always written as is) and `BOOLEAN` (empty is false). Conversions:
//!
//! * `BOOLEAN`: `1/true/t/yes/y/on` are true, `0/false/f/no/n/off` and the empty string are
//!   false, other numbers are true when nonzero, and other strings are true.
//! * `DATE`, `TIMESTAMP`: an integer is a Unix timestamp (seconds; milliseconds, microseconds
//!   and nanoseconds are detected by magnitude), a string is parsed as a date or date-time
//!   (RFC 3339, `YYYY-MM-DD[ HH:MM[:SS[.f]]]`, ...); date-times without an offset are UTC.
//! * `TIME`: an integer is the number of seconds since midnight, a string is `HH:MM[:SS[.f]]`
//!   (or a date-time, whose UTC time of day is used).
//! * `JSON`: the value must be valid JSON; `UUID`: any format accepted by `uuid::Uuid::parse_str`.

use std::io::{self, Write};
use std::sync::Arc;

use ::parquet::basic::{Compression, LogicalType, Repetition, TimeUnit, Type as PhysicalType};
use ::parquet::data_type::{
    BoolType, ByteArray, ByteArrayType, DoubleType, FixedLenByteArray, FixedLenByteArrayType,
    FloatType, Int32Type, Int64Type,
};
use ::parquet::file::properties::WriterProperties;
use ::parquet::file::writer::SerializedFileWriter;
use ::parquet::schema::types::Type;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

use super::writers::FileFactory;
use super::command::CommandEnv;
use crate::common::FileSpec;

/// The number of rows buffered before a row group is written.
const ROW_GROUP_SIZE: usize = 64 * 1024;

const MICROS_PER_SEC: i64 = 1_000_000;
const SECS_PER_DAY: i64 = 86_400;

/// Wraps a factory so that stdout is written as a Parquet file (`-o parquet`).
#[derive(Clone)]
pub struct ParquetFactory<F>(pub F);

impl<F: FileFactory> FileFactory for ParquetFactory<F> {
    type Output = F::Output;
    type Stdout = ParquetWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<std::process::Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &str, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        ParquetWriter::new(self.0.stdout())
    }
}

/// The type of a column, from the `:TYPE` suffix of its name in the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    String,
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    Float,
    Double,
    Date,
    Time,
    Timestamp,
    Json,
    Uuid,
}

impl Kind {
    fn parse(name: &str) -> Option<Kind> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "string" | "str" | "utf8" | "text" | "varchar" => Kind::String,
            "boolean" | "bool" => Kind::Bool,
            "int8" | "tinyint" => Kind::Int8,
            "int16" | "smallint" => Kind::Int16,
            "int32" | "int" | "integer" => Kind::Int32,
            "int64" | "bigint" | "long" => Kind::Int64,
            "float" | "float32" | "real" => Kind::Float,
            "double" | "float64" => Kind::Double,
            "date" => Kind::Date,
            "time" => Kind::Time,
            "timestamp" | "datetime" => Kind::Timestamp,
            "json" => Kind::Json,
            "uuid" => Kind::Uuid,
            _ => return None,
        })
    }

    fn parquet_type(self, name: &str) -> ::parquet::errors::Result<Type> {
        use PhysicalType::*;
        let (physical, logical) = match self {
            Kind::String => (BYTE_ARRAY, Some(LogicalType::String)),
            Kind::Bool => (BOOLEAN, None),
            Kind::Int8 => (INT32, Some(LogicalType::integer(8, true))),
            Kind::Int16 => (INT32, Some(LogicalType::integer(16, true))),
            Kind::Int32 => (INT32, None),
            Kind::Int64 => (INT64, None),
            Kind::Float => (FLOAT, None),
            Kind::Double => (DOUBLE, None),
            Kind::Date => (INT32, Some(LogicalType::Date)),
            Kind::Time => (INT64, Some(LogicalType::time(false, TimeUnit::MICROS))),
            Kind::Timestamp => (INT64, Some(LogicalType::timestamp(true, TimeUnit::MICROS))),
            Kind::Json => (BYTE_ARRAY, Some(LogicalType::Json)),
            Kind::Uuid => (FIXED_LEN_BYTE_ARRAY, Some(LogicalType::Uuid)),
        };
        let mut builder = Type::primitive_type_builder(name, physical)
            .with_repetition(Repetition::OPTIONAL)
            .with_logical_type(logical);
        if self == Kind::Uuid {
            builder = builder.with_length(16);
        }
        builder.build()
    }
}

/// The non-null values of a column in the current row group.
enum Values {
    Bool(Vec<bool>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Bytes(Vec<ByteArray>),
    Fixed(Vec<FixedLenByteArray>),
}

struct Column {
    name: String,
    kind: Kind,
    values: Values,
    /// Definition levels: 1 for a value, 0 for null.
    defs: Vec<i16>,
    /// The number of non-empty values that could not be converted and were written as null.
    invalid: usize,
}

impl Column {
    fn new(name: String, kind: Kind) -> Column {
        let values = match kind {
            Kind::Bool => Values::Bool(Vec::new()),
            Kind::Int8 | Kind::Int16 | Kind::Int32 | Kind::Date => Values::I32(Vec::new()),
            Kind::Int64 | Kind::Time | Kind::Timestamp => Values::I64(Vec::new()),
            Kind::Float => Values::F32(Vec::new()),
            Kind::Double => Values::F64(Vec::new()),
            Kind::String | Kind::Json => Values::Bytes(Vec::new()),
            Kind::Uuid => Values::Fixed(Vec::new()),
        };
        Column { name, kind, values, defs: Vec::new(), invalid: 0 }
    }

    /// Appends a value, given as TSV-escaped text.
    fn push(&mut self, raw: &[u8]) {
        let text = unescape_tsv(raw);
        let defined = match (&mut self.values, self.kind) {
            (Values::Bytes(v), Kind::String) => {
                v.push(ByteArray::from(text.into_owned().into_bytes()));
                true
            }
            (Values::Bool(v), _) => {
                v.push(crate::runtime::math_util::parse_bool(&text));
                true
            }
            _ if text.trim().is_empty() => {
                self.defs.push(0);
                return;
            }
            (Values::I32(v), kind) => push_some(v, match kind {
                Kind::Int8 => parse_int(&text).and_then(|i| i8::try_from(i).ok()).map(i32::from),
                Kind::Int16 => parse_int(&text).and_then(|i| i16::try_from(i).ok()).map(i32::from),
                Kind::Date => parse_date(&text),
                _ => parse_int(&text).and_then(|i| i32::try_from(i).ok()),
            }),
            (Values::I64(v), kind) => push_some(v, match kind {
                Kind::Time => parse_time(&text),
                Kind::Timestamp => parse_timestamp(&text),
                _ => parse_int(&text),
            }),
            (Values::F32(v), _) => push_some(v, parse_float(&text).map(|f| f as f32)),
            (Values::F64(v), _) => push_some(v, parse_float(&text)),
            (Values::Bytes(v), _) => {
                // JSON. TSV escaping turns tabs and newlines into `\t` and `\n`, which are also
                // JSON escapes, so the value is first validated as printed.
                let json = [raw, text.as_bytes()]
                    .into_iter()
                    .find(|s| serde_json::from_slice::<serde_json::Value>(s).is_ok());
                push_some(v, json.map(|s| ByteArray::from(s.to_vec())))
            }
            (Values::Fixed(v), _) => push_some(
                v,
                uuid::Uuid::parse_str(text.trim())
                    .ok()
                    .map(|u| FixedLenByteArray::from(u.as_bytes().to_vec())),
            ),
        };
        if !defined {
            self.invalid += 1;
        }
        self.defs.push(defined as i16);
    }

    fn write<W: Write + Send>(
        &mut self,
        rg: &mut ::parquet::file::writer::SerializedRowGroupWriter<'_, W>,
    ) -> ::parquet::errors::Result<()> {
        let mut cw = rg.next_column()?.expect("column count matches the schema");
        let defs = Some(&self.defs[..]);
        match &mut self.values {
            Values::Bool(v) => cw.typed::<BoolType>().write_batch(v, defs, None)?,
            Values::I32(v) => cw.typed::<Int32Type>().write_batch(v, defs, None)?,
            Values::I64(v) => cw.typed::<Int64Type>().write_batch(v, defs, None)?,
            Values::F32(v) => cw.typed::<FloatType>().write_batch(v, defs, None)?,
            Values::F64(v) => cw.typed::<DoubleType>().write_batch(v, defs, None)?,
            Values::Bytes(v) => cw.typed::<ByteArrayType>().write_batch(v, defs, None)?,
            Values::Fixed(v) => cw.typed::<FixedLenByteArrayType>().write_batch(v, defs, None)?,
        };
        cw.close()?;
        match &mut self.values {
            Values::Bool(v) => v.clear(),
            Values::I32(v) => v.clear(),
            Values::I64(v) => v.clear(),
            Values::F32(v) => v.clear(),
            Values::F64(v) => v.clear(),
            Values::Bytes(v) => v.clear(),
            Values::Fixed(v) => v.clear(),
        }
        self.defs.clear();
        Ok(())
    }
}

fn push_some<T>(v: &mut Vec<T>, x: Option<T>) -> bool {
    match x {
        Some(x) => {
            v.push(x);
            true
        }
        None => false,
    }
}

/// Reverses the TSV escaping of `print` under `-o tsv`.
fn unescape_tsv(raw: &[u8]) -> std::borrow::Cow<'_, str> {
    match String::from_utf8_lossy(raw) {
        std::borrow::Cow::Borrowed(text) => crate::runtime::str_escape::unescape_tsv(text),
        std::borrow::Cow::Owned(text) => {
            crate::runtime::str_escape::unescape_tsv(&text).into_owned().into()
        }
    }
}

fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    s.parse::<i64>().ok().or_else(|| {
        let f = s.parse::<f64>().ok().filter(|f| f.is_finite())?;
        (f >= i64::MIN as f64 && f <= i64::MAX as f64).then(|| f.trunc() as i64)
    })
}

fn parse_float(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok()
}

/// Microseconds since the Unix epoch from a numeric Unix timestamp: seconds, or milliseconds,
/// microseconds or nanoseconds when the value is too large to be a timestamp in seconds.
fn epoch_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        let abs = i.unsigned_abs();
        return if abs >= 100_000_000_000_000_000 {
            Some(i / 1000)
        } else if abs >= 100_000_000_000_000 {
            Some(i)
        } else if abs >= 100_000_000_000 {
            i.checked_mul(1000)
        } else {
            i.checked_mul(MICROS_PER_SEC)
        };
    }
    let f = s.parse::<f64>().ok().filter(|f| f.is_finite())?;
    let micros = (f * MICROS_PER_SEC as f64).round();
    (micros.abs() < i64::MAX as f64).then(|| micros as i64)
}

const DATE_TIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    "%Y/%m/%d %H:%M:%S%.f",
    "%Y/%m/%d %H:%M",
];
const DATE_TIME_TZ_FORMATS: &[&str] = &["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f %z"];
const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%Y/%m/%d"];

/// Microseconds since the Unix epoch from a date-time or date string; naive values are UTC.
fn parse_date_time_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_micros());
    }
    for fmt in DATE_TIME_TZ_FORMATS {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Some(dt.timestamp_micros());
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Some(dt.timestamp_micros());
    }
    for fmt in DATE_TIME_FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt.and_utc().timestamp_micros());
        }
    }
    for fmt in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros());
        }
    }
    None
}

fn parse_timestamp(s: &str) -> Option<i64> {
    epoch_micros(s).or_else(|| parse_date_time_str(s))
}

/// Days since the Unix epoch.
fn parse_date(s: &str) -> Option<i32> {
    let micros = parse_timestamp(s)?;
    i32::try_from(micros.div_euclid(MICROS_PER_SEC * SECS_PER_DAY)).ok()
}

/// Microseconds since midnight.
fn parse_time(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(f) = parse_float(t) {
        let micros = (f * MICROS_PER_SEC as f64).round();
        return (0.0..(SECS_PER_DAY * MICROS_PER_SEC) as f64)
            .contains(&micros)
            .then(|| micros as i64);
    }
    for fmt in ["%H:%M:%S%.f", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(t, fmt) {
            return Some(
                time.num_seconds_from_midnight() as i64 * MICROS_PER_SEC
                    + (time.nanosecond() / 1000) as i64,
            );
        }
    }
    parse_date_time_str(t).map(|micros| micros.rem_euclid(SECS_PER_DAY * MICROS_PER_SEC))
}

/// Parses a header field: `name` or `name:TYPE`.
fn parse_column(index: usize, raw: &[u8]) -> io::Result<Column> {
    let text = unescape_tsv(raw);
    let (name, kind) = match text.rsplit_once(':') {
        Some((name, ty)) => match Kind::parse(ty) {
            Some(kind) => (name, kind),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown parquet column type {:?} in {:?}; expected one of STRING, \
                         BOOLEAN, INT8, INT16, INT32, INT64, FLOAT, DOUBLE, DATE, TIME, \
                         TIMESTAMP, JSON or UUID",
                        ty, text
                    ),
                ))
            }
        },
        None => (&*text, Kind::String),
    };
    let name = name.trim();
    let name = if name.is_empty() {
        format!("c{}", index + 1)
    } else {
        name.to_string()
    };
    Ok(Column::new(name, kind))
}

struct Encoder {
    columns: Vec<Column>,
    /// Writes into memory; the bytes are moved to the output after each row group.
    writer: SerializedFileWriter<Vec<u8>>,
    rows: usize,
}

fn to_io(e: ::parquet::errors::ParquetError) -> io::Error {
    io::Error::other(format!("failed to write parquet: {}", e))
}

impl Encoder {
    fn new(header: &[u8]) -> io::Result<Encoder> {
        let columns = header
            .split(|b| *b == b'\t')
            .enumerate()
            .map(|(i, f)| parse_column(i, f))
            .collect::<io::Result<Vec<_>>>()?;
        let fields = columns
            .iter()
            .map(|c| c.kind.parquet_type(&c.name).map(Arc::new))
            .collect::<::parquet::errors::Result<Vec<_>>>()
            .map_err(to_io)?;
        let schema = Type::group_type_builder("schema")
            .with_fields(fields)
            .build()
            .map_err(to_io)?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .set_created_by(format!("zawk version {}", env!("CARGO_PKG_VERSION")))
            .build();
        let writer = SerializedFileWriter::new(Vec::new(), Arc::new(schema), Arc::new(props))
            .map_err(to_io)?;
        Ok(Encoder { columns, writer, rows: 0 })
    }

    fn push_row(&mut self, line: &[u8]) {
        let mut cells = line.split(|b| *b == b'\t');
        for col in self.columns.iter_mut() {
            // Missing trailing fields are empty; extra fields are ignored.
            col.push(cells.next().unwrap_or(b""));
        }
        self.rows += 1;
    }

    /// Writes the buffered rows as a row group, moving the encoded bytes to `out`.
    fn flush_rows(&mut self, out: &mut impl Write) -> io::Result<()> {
        if self.rows > 0 {
            let mut rg = self.writer.next_row_group().map_err(to_io)?;
            for col in self.columns.iter_mut() {
                col.write(&mut rg).map_err(to_io)?;
            }
            rg.close().map_err(to_io)?;
            self.rows = 0;
        }
        self.writer.flush()?;
        out.write_all(&std::mem::take(self.writer.inner_mut()))
    }

    fn finish(mut self, out: &mut impl Write) -> io::Result<()> {
        self.flush_rows(out)?;
        for col in &self.columns {
            if col.invalid > 0 {
                eprintln!(
                    "zawk: parquet column {:?} ({:?}): {} invalid value(s) written as null",
                    col.name, col.kind, col.invalid
                );
            }
        }
        let bytes = self.writer.into_inner().map_err(to_io)?;
        out.write_all(&bytes)?;
        out.flush()
    }
}

/// Converts TSV records written to it into a Parquet file written to `inner`; see the module
/// documentation.
pub struct ParquetWriter<W: Write> {
    inner: W,
    /// The current, not yet newline-terminated, record.
    pending: Vec<u8>,
    /// Created from the first record (the header).
    encoder: Option<Encoder>,
    /// Set after an error, which has already been reported; later records are dropped.
    failed: bool,
}

impl<W: Write> ParquetWriter<W> {
    pub fn new(inner: W) -> ParquetWriter<W> {
        ParquetWriter {
            inner,
            pending: Vec::new(),
            encoder: None,
            failed: false,
        }
    }

    fn write_record(&mut self, line: &[u8]) -> io::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        match &mut self.encoder {
            None => match Encoder::new(line) {
                Ok(enc) => self.encoder = Some(enc),
                // An invalid header is a usage error rather than an output error (which ends the
                // program silently), and nothing has been written yet.
                Err(e) if e.kind() == io::ErrorKind::InvalidInput && !cfg!(test) => {
                    eprintln!("{}", crate::common::fatal_message(&e.to_string()));
                    std::process::exit(2);
                }
                Err(e) => return Err(e),
            },
            Some(enc) => {
                enc.push_row(line);
                if enc.rows >= ROW_GROUP_SIZE {
                    enc.flush_rows(&mut self.inner)?;
                }
            }
        }
        Ok(())
    }

    fn checked<T>(&mut self, r: io::Result<T>) -> io::Result<()> {
        if let Err(e) = r {
            self.failed = true;
            return Err(e);
        }
        Ok(())
    }

    /// Writes the remaining rows and the file footer.
    fn finish(&mut self) -> io::Result<()> {
        if self.failed {
            return Ok(());
        }
        // A final record without a trailing newline (e.g. from printf) is still a row.
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let r = self.write_record(&line);
            self.checked(r)?;
        }
        match self.encoder.take() {
            Some(enc) => enc.finish(&mut self.inner),
            None => self.inner.flush(),
        }
    }
}

impl<W: Write> Write for ParquetWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.failed {
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
            self.checked(r)?;
            rest = &rest[pos + 1..];
        }
        self.pending.extend_from_slice(rest);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        // Nothing is written before a row group is complete.
        Ok(())
    }
}

impl<W: Write> Drop for ParquetWriter<W> {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            eprintln!("zawk: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::parquet::file::reader::{FileReader, SerializedFileReader};

    fn rows(data: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        {
            let mut w = ParquetWriter::new(&mut out);
            w.write_all(data).unwrap();
        }
        let reader = SerializedFileReader::new(bytes::Bytes::from(out)).unwrap();
        reader
            .get_row_iter(None)
            .unwrap()
            .map(|r| r.unwrap().to_string())
            .collect()
    }

    #[test]
    fn typed_columns() {
        let got = rows(
            b"name\tage:int32\tok:Boolean\td:DATE\tts:timestamp\tt:Time\tj:json\tu:UUID\n\
              Alice\t30\ttrue\t2024-01-02\t2024-01-02 03:04:05\t03:04:05\t{\"a\":1}\t67e55044-10b1-426f-9247-bb680e5fe0c8\n\
              Bob\tx\t0\t86400\t1700000000000\t3600\tnot json\t\n\
              \t\t\t\t\t\t\t",
        );
        assert_eq!(got.len(), 3);
        assert!(got[0].contains("name: \"Alice\""), "{}", got[0]);
        assert!(got[0].contains("age: 30"), "{}", got[0]);
        assert!(got[0].contains("ok: true"), "{}", got[0]);
        assert!(got[0].contains("d: 2024-01-02"), "{}", got[0]);
        assert!(got[0].contains("ts: 2024-01-02 03:04:05"), "{}", got[0]);
        assert!(got[0].contains("j: {\"a\":1}") || got[0].contains("j: \"{"), "{}", got[0]);
        assert!(got[1].contains("age: null"), "{}", got[1]);
        assert!(got[1].contains("ok: false"), "{}", got[1]);
        assert!(got[1].contains("d: 1970-01-02"), "{}", got[1]);
        assert!(got[1].contains("ts: 2023-11-14 22:13:20"), "{}", got[1]);
        assert!(got[1].contains("j: null"), "{}", got[1]);
        assert!(got[1].contains("u: null"), "{}", got[1]);
        assert!(got[2].contains("name: \"\""), "{}", got[2]);
        assert!(got[2].contains("ok: false"), "{}", got[2]);
    }

    #[test]
    fn unknown_type() {
        let mut w = ParquetWriter::new(Vec::new());
        assert!(w.write_all(b"a:nope\n").is_err());
    }

    #[test]
    fn conversions() {
        use crate::runtime::math_util::parse_bool;
        assert!(parse_bool("yes") && parse_bool("2") && !parse_bool("0.0") && !parse_bool("FALSE"));
        assert!(!parse_bool("0x0") && !parse_bool(" ") && !parse_bool(" -0 ") && parse_bool("abc"));
        assert_eq!(parse_time("01:00"), Some(3600 * MICROS_PER_SEC));
        assert_eq!(parse_timestamp("1970-01-01T00:00:01Z"), Some(MICROS_PER_SEC));
        assert_eq!(parse_timestamp("1000"), Some(1000 * MICROS_PER_SEC));
        assert_eq!(parse_date("1970-01-03"), Some(2));
    }
}
