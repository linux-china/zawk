//! Apache Parquet output (`-o parquet`).
//!
//! Records printed to stdout are written as TSV (as for `-o tsv`) and converted to a Parquet file
//! by `ParquetWriter`. The first record is the header: each field is a column name, optionally
//! followed by `:TYPE` (case-insensitive, e.g. `age:INT32`), and columns without a type are
//! strings. The following records are data rows. Rows are buffered and written in row groups;
//! the file footer is written when stdout is closed, at the end of the program.
//!
//! All columns are OPTIONAL. Empty or unconvertible values are written as null, except for
//! `STRING` (always written as is); unconvertible values are counted in a warning. A field that is
//! the `--null` text is null in every column. Conversions:
//!
//! * `BOOLEAN`: `true/t/yes/y/on` (any case) and nonzero numbers are true, `false/f/no/n/off` and
//!   zero are false; other values (such as `maybe`) are invalid and written as null.
//! * `DATE`, `TIMESTAMP`: an integer is a Unix timestamp (seconds; milliseconds, microseconds
//!   and nanoseconds are detected by magnitude), a string is parsed as a date or date-time
//!   (RFC 3339, `YYYY-MM-DD[ HH:MM[:SS[.f]]]`, ...); date-times without an offset are UTC.
//! * `TIME`: an integer is the number of seconds since midnight, a string is `HH:MM[:SS[.f]]`
//!   (or a date-time, whose UTC time of day is used).
//! * `JSON`: the value must be valid JSON; `UUID`: any format accepted by `uuid::Uuid::parse_str`.
//!
//! A column of type `AUTO` (and, with `--infer-types`, a column without a type) gets its type from
//! the values of the first row group (up to `ROW_GROUP_SIZE` rows), which are buffered until the
//! schema is known. Ignoring empty and null values, it is `INT64` if every value is an integer as
//! JSON writes it (`007` is not), `DOUBLE` if every value is such a number, `BOOLEAN` if every value
//! is `true` or `false`, `DATE` if every value is `YYYY-MM-DD`, and `STRING` otherwise. Empty values
//! of an inferred column are null, also when it is `STRING`; later values that do not fit the type
//! are null and reported.

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

use super::command::CommandEnv;
use super::writers::FileFactory;
use crate::common::FileSpec;

/// The number of rows buffered before a row group is written.
const ROW_GROUP_SIZE: usize = 64 * 1024;

#[cfg(test)]
use super::typed_value::MICROS_PER_SEC;
use super::typed_value::{
    Inferred, infer_type, parse_date, parse_f32, parse_float, parse_int, parse_parquet_bool,
    parse_time, parse_timestamp, unescape_tsv,
};

/// Wraps a factory so that stdout is written as a Parquet file (`-o parquet`).
///
/// With `--infer-types` (the second field), columns without a `:TYPE` suffix are `:AUTO`.
#[derive(Clone)]
pub struct ParquetFactory<F>(pub F, pub bool);

impl<F: FileFactory> FileFactory for ParquetFactory<F> {
    type Output = F::Output;
    type Stdout = ParquetWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<std::process::Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &std::path::Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        ParquetWriter::new(self.0.stdout(), self.1)
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
    /// The number of rows the type was inferred from, for an `AUTO` column.
    inferred_from: Option<usize>,
}

impl Column {
    fn new(name: String, kind: Kind, inferred_from: Option<usize>) -> Column {
        let values = match kind {
            Kind::Bool => Values::Bool(Vec::new()),
            Kind::Int8 | Kind::Int16 | Kind::Int32 | Kind::Date => Values::I32(Vec::new()),
            Kind::Int64 | Kind::Time | Kind::Timestamp => Values::I64(Vec::new()),
            Kind::Float => Values::F32(Vec::new()),
            Kind::Double => Values::F64(Vec::new()),
            Kind::String | Kind::Json => Values::Bytes(Vec::new()),
            Kind::Uuid => Values::Fixed(Vec::new()),
        };
        Column {
            name,
            kind,
            values,
            defs: Vec::new(),
            invalid: 0,
            inferred_from,
        }
    }

    /// Appends a value, given as TSV-escaped text.
    fn push(&mut self, raw: &[u8]) {
        if crate::runtime::str_escape::is_tsv_null(raw) {
            self.defs.push(0);
            return;
        }
        let text = unescape_tsv(raw);
        // Empty values of an inferred column are null; a STRING column keeps blank (whitespace)
        // values, other types treat them as empty.
        let empty = text.is_empty() || (self.kind != Kind::String && text.trim().is_empty());
        if self.inferred_from.is_some() && empty {
            self.defs.push(0);
            return;
        }
        let defined = match (&mut self.values, self.kind) {
            (Values::Bytes(v), Kind::String) => {
                v.push(ByteArray::from(text.into_owned().into_bytes()));
                true
            }
            _ if text.trim().is_empty() => {
                self.defs.push(0);
                return;
            }
            (Values::Bool(v), _) => push_some(v, parse_parquet_bool(&text)),
            (Values::I32(v), kind) => push_some(
                v,
                match kind {
                    Kind::Int8 => parse_int(&text)
                        .and_then(|i| i8::try_from(i).ok())
                        .map(i32::from),
                    Kind::Int16 => parse_int(&text)
                        .and_then(|i| i16::try_from(i).ok())
                        .map(i32::from),
                    Kind::Date => parse_date(&text),
                    _ => parse_int(&text).and_then(|i| i32::try_from(i).ok()),
                },
            ),
            (Values::I64(v), kind) => push_some(
                v,
                match kind {
                    Kind::Time => parse_time(&text),
                    Kind::Timestamp => parse_timestamp(&text),
                    _ => parse_int(&text),
                },
            ),
            (Values::F32(v), _) => push_some(v, parse_f32(&text)),
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
            Values::Fixed(v) => cw
                .typed::<FixedLenByteArrayType>()
                .write_batch(v, defs, None)?,
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

/// A column of the header: its name, and its type (`None` for `AUTO`).
type ColumnSpec = (String, Option<Kind>);

/// Parses a header field: `name` or `name:TYPE`; a column without a type is `AUTO` if `infer`.
fn parse_column(index: usize, raw: &[u8], infer: bool) -> io::Result<ColumnSpec> {
    let text = unescape_tsv(raw);
    let default = if infer { None } else { Some(Kind::String) };
    let (name, kind) = match text.rsplit_once(':') {
        Some((name, ty)) if ty.trim().eq_ignore_ascii_case("auto") => (name, None),
        Some((name, ty)) => match Kind::parse(ty) {
            Some(kind) => (name, Some(kind)),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "unknown parquet column type {:?} in {:?}; expected one of STRING, \
                         BOOLEAN, INT8, INT16, INT32, INT64, FLOAT, DOUBLE, DATE, TIME, \
                         TIMESTAMP, JSON, UUID or AUTO",
                        ty, text
                    ),
                ));
            }
        },
        None => (&*text, default),
    };
    let name = name.trim();
    let name = if name.is_empty() {
        format!("c{}", index + 1)
    } else {
        name.to_string()
    };
    Ok((name, kind))
}

fn parse_header(header: &[u8], infer: bool) -> io::Result<Vec<ColumnSpec>> {
    header
        .split(|b| *b == b'\t')
        .enumerate()
        .map(|(i, f)| parse_column(i, f, infer))
        .collect()
}

/// The Parquet type of an `AUTO` column.
fn inferred_kind(sample: &[Vec<u8>], index: usize) -> Kind {
    match infer_type(sample, index) {
        Inferred::String => Kind::String,
        Inferred::Int64 => Kind::Int64,
        Inferred::Double => Kind::Double,
        Inferred::Bool => Kind::Bool,
        Inferred::Date => Kind::Date,
    }
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
    /// Creates the encoder for the header `specs`; the types of `AUTO` columns are inferred from
    /// `sample`, the first rows, which the caller then pushes.
    fn new(specs: Vec<ColumnSpec>, sample: &[Vec<u8>]) -> io::Result<Encoder> {
        let columns: Vec<Column> = specs
            .into_iter()
            .enumerate()
            .map(|(i, (name, kind))| match kind {
                Some(kind) => Column::new(name, kind, None),
                None => Column::new(name, inferred_kind(sample, i), Some(sample.len())),
            })
            .collect();
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
        Ok(Encoder {
            columns,
            writer,
            rows: 0,
        })
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
                let inferred = match col.inferred_from {
                    Some(n) => format!(
                        ", inferred from the first {} row(s); give the column a :TYPE suffix",
                        n
                    ),
                    None => String::new(),
                };
                eprintln!(
                    "zawk: parquet column {:?} ({:?}{}): {} invalid value(s) written as null",
                    col.name, col.kind, inferred, col.invalid
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
    /// Created from the first record (the header), or after the sample with `AUTO` columns.
    encoder: Option<Encoder>,
    /// The header while the rows that `AUTO` column types are inferred from are buffered.
    header: Option<Vec<ColumnSpec>>,
    /// The buffered rows, while `header` is set.
    sample: Vec<Vec<u8>>,
    /// Whether columns without a type are `AUTO` (`--infer-types`).
    infer_types: bool,
    /// Set after an error, which has already been reported; later records are dropped.
    failed: bool,
}

impl<W: Write> ParquetWriter<W> {
    pub fn new(inner: W, infer_types: bool) -> ParquetWriter<W> {
        ParquetWriter {
            inner,
            pending: Vec::new(),
            encoder: None,
            header: None,
            sample: Vec::new(),
            infer_types,
            failed: false,
        }
    }

    /// Creates the encoder from the buffered header and sample, and writes the sample.
    fn start(&mut self, specs: Vec<ColumnSpec>) -> io::Result<()> {
        let sample = std::mem::take(&mut self.sample);
        let mut enc = Encoder::new(specs, &sample)?;
        for row in &sample {
            enc.push_row(row);
        }
        if enc.rows >= ROW_GROUP_SIZE {
            enc.flush_rows(&mut self.inner)?;
        }
        self.encoder = Some(enc);
        Ok(())
    }

    fn write_record(&mut self, line: &[u8]) -> io::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(specs) = &self.header {
            self.sample.push(line.to_vec());
            if self.sample.len() >= ROW_GROUP_SIZE {
                let specs = specs.clone();
                self.header = None;
                self.start(specs)?;
            }
            return Ok(());
        }
        match &mut self.encoder {
            None => match parse_header(line, self.infer_types) {
                Ok(specs) if specs.iter().any(|(_, kind)| kind.is_none()) => {
                    self.header = Some(specs);
                }
                Ok(specs) => self.start(specs)?,
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
        if let Some(specs) = self.header.take() {
            let r = self.start(specs);
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
            crate::runtime::writers::report_stdout_render_error(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::parquet::file::reader::{FileReader, SerializedFileReader};

    fn rows(data: &[u8]) -> Vec<String> {
        rows_with(data, false)
    }

    fn rows_with(data: &[u8], infer: bool) -> Vec<String> {
        let mut out = Vec::new();
        {
            let mut w = ParquetWriter::new(&mut out, infer);
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
              Bob\tx\t0\t19700102\t1700000000000\t3600\tnot json\t\n\
              \t\t\t\t\t\t\t",
        );
        assert_eq!(got.len(), 3);
        assert!(got[0].contains("name: \"Alice\""), "{}", got[0]);
        assert!(got[0].contains("age: 30"), "{}", got[0]);
        assert!(got[0].contains("ok: true"), "{}", got[0]);
        assert!(got[0].contains("d: 2024-01-02"), "{}", got[0]);
        assert!(got[0].contains("ts: 2024-01-02 03:04:05"), "{}", got[0]);
        assert!(
            got[0].contains("j: {\"a\":1}") || got[0].contains("j: \"{"),
            "{}",
            got[0]
        );
        assert!(got[1].contains("age: null"), "{}", got[1]);
        assert!(got[1].contains("ok: false"), "{}", got[1]);
        assert!(got[1].contains("d: 1970-01-02"), "{}", got[1]);
        assert!(got[1].contains("ts: 2023-11-14 22:13:20"), "{}", got[1]);
        assert!(got[1].contains("j: null"), "{}", got[1]);
        assert!(got[1].contains("u: null"), "{}", got[1]);
        assert!(got[2].contains("name: \"\""), "{}", got[2]);
        assert!(got[2].contains("ok: null"), "{}", got[2]);
    }

    #[test]
    fn boolean_column() {
        let mut out = Vec::new();
        {
            let mut w = ParquetWriter::new(&mut out, false);
            w.write_all(b"ok:boolean\nYes\n0.0\n2\nOFF\nmaybe\n\n \n")
                .unwrap();
            assert_eq!(w.encoder.as_ref().unwrap().columns[0].invalid, 1);
        }
        let reader = SerializedFileReader::new(bytes::Bytes::from(out)).unwrap();
        let got: Vec<String> = reader
            .get_row_iter(None)
            .unwrap()
            .map(|r| r.unwrap().to_string())
            .collect();
        assert_eq!(
            got,
            [
                "{ok: true}",
                "{ok: false}",
                "{ok: true}",
                "{ok: false}",
                "{ok: null}",
                "{ok: null}",
                "{ok: null}",
            ]
        );
        assert_eq!(parse_parquet_bool(" ✓ "), Some(true));
        assert_eq!(parse_parquet_bool("NaN"), None);
    }

    #[test]
    fn auto_columns() {
        let got = rows_with(
            b"id\tname\tscore\tok\td\tzip\tbig\tx:string\tn:auto\n\
              1\tJackie\t1.5\ttrue\t2024-01-02\t007\t12345678901234567890\t1\t\n\
              2\t\t2\tFALSE\t2024-01-03\t123\t1\t2\t\n\
              \t\t\t\t\t\t\t\t\n",
            true,
        );
        assert_eq!(
            got,
            [
                "{id: 1, name: \"Jackie\", score: 1.5, ok: true, d: 2024-01-02, zip: \"007\", \
                 big: 1.2345678901234567E19, x: \"1\", n: null}",
                "{id: 2, name: null, score: 2.0, ok: false, d: 2024-01-03, zip: \"123\", \
                 big: 1.0, x: \"2\", n: null}",
                "{id: null, name: null, score: null, ok: null, d: null, zip: null, big: null, \
                 x: \"\", n: null}",
            ]
        );
        // An integer beyond INT64 makes the column DOUBLE; a column without values is STRING.
        // Empty values of an inferred column are null, also for STRING; a blank STRING value is
        // kept, and an explicit `:string` column keeps empty values.
        // Without --infer-types only `:auto` columns are inferred.
        let got = rows_with(b"id\tn:AUTO\n1\t2\n", false);
        assert_eq!(got, ["{id: \"1\", n: 2}"]);
        assert_eq!(rows_with(b"n:auto\n", false), Vec::<String>::new());
    }

    #[test]
    fn unknown_type() {
        let mut w = ParquetWriter::new(Vec::new(), false);
        assert!(w.write_all(b"a:nope\n").is_err());
    }

    #[test]
    fn conversions() {
        use crate::runtime::math_util::parse_bool;
        assert!(parse_bool("yes") && parse_bool("2") && !parse_bool("0.0") && !parse_bool("FALSE"));
        assert!(!parse_bool("0x0") && !parse_bool(" ") && !parse_bool(" -0 ") && parse_bool("abc"));
        assert_eq!(parse_time("01:00"), Some(3600 * MICROS_PER_SEC));
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:01Z"),
            Some(MICROS_PER_SEC)
        );
        assert_eq!(parse_timestamp("1000"), Some(1000 * MICROS_PER_SEC));
        assert_eq!(parse_date("1970-01-03"), Some(2));
        assert_eq!(parse_date(" 20240115 "), Some(19737));
        assert_eq!(parse_date("2024-01-15T23:59:59Z"), Some(19737));
        assert_eq!(parse_date("1960-01-01"), Some(-3653));
        for bad in ["2024", "20241345", "1705276800", "-1", "1.5", ""] {
            assert_eq!(parse_date(bad), None, "{bad}");
        }
        assert_eq!(parse_int(" 42 "), Some(42));
        assert_eq!(parse_int("1e3"), Some(1000));
        assert_eq!(parse_int("-2.0"), Some(-2));
        assert_eq!(parse_int("-9223372036854775808.0"), Some(i64::MIN));
        assert_eq!(parse_int("1.5"), None);
        assert_eq!(parse_int("9223372036854775808"), None);
        assert_eq!(parse_int("9.3e18"), None);
        assert_eq!(parse_int("-9.3e18"), None);
    }

    #[test]
    fn int_columns_reject_out_of_range_and_fractions() {
        assert_eq!(
            rows(b"id:INT64\ti8:INT8\ti32:INT32\n9223372036854775808\t1.5\t2147483648\n1e3\t127\t-2.0\n"),
            [
                "{id: null, i8: null, i32: null}",
                "{id: 1000, i8: 127, i32: -2}",
            ]
        );
    }

    #[test]
    fn int_bounds_are_exact() {
        // Each integer type at its bounds and one past them, in plain, decimal and exponent
        // forms; values f64 cannot hold exactly are not rounded into a different integer.
        for (min, max) in [
            (i64::from(i8::MIN), i64::from(i8::MAX)),
            (i64::from(i16::MIN), i64::from(i16::MAX)),
            (i64::from(i32::MIN), i64::from(i32::MAX)),
        ] {
            for v in [min, max] {
                assert_eq!(parse_int(&v.to_string()), Some(v));
                assert_eq!(parse_int(&format!("{}.0", v)), Some(v));
            }
        }
        assert_eq!(parse_int("9223372036854775807"), Some(i64::MAX));
        assert_eq!(parse_int("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(parse_int("-9223372036854775809"), None);
        assert_eq!(parse_int("-9223372036854775809.0"), None);
        assert_eq!(parse_int("9223372036854775807.0"), Some(i64::MAX));
        assert_eq!(parse_int("9223372036854775808.0"), None);
        assert_eq!(parse_int("9007199254740993.0"), Some(9007199254740993));
        assert_eq!(parse_int("9.007199254740993e15"), Some(9007199254740993));
        assert_eq!(parse_int("1.5e1"), Some(15));
        assert_eq!(parse_int("+1E2"), Some(100));
        assert_eq!(parse_int("120e-1"), Some(12));
        assert_eq!(parse_int("1e-1"), None);
        assert_eq!(parse_int("0e999999"), Some(0));
        assert_eq!(parse_int("1e19"), None);
        assert_eq!(parse_int("1e99999999999"), None);
        for bad in [
            "", ".", "e5", "1e", "1.2.3", "0x10", "1_000", "inf", "nan", "- 1", "1e+",
        ] {
            assert_eq!(parse_int(bad), None, "{:?}", bad);
        }
    }

    #[test]
    fn typed_column_bounds() {
        let tsv = concat!(
            "i8:INT8\ti16:INT16\ti32:INT32\ti64:INT64\tf:FLOAT\td:DOUBLE\n",
            "-128\t-32768\t-2147483648\t-9223372036854775808\t3.4e38\t1.7e308\n",
            "127\t32767\t2147483647\t9223372036854775807\t-3.4e38\t-1.7e308\n",
            "-129\t-32769\t-2147483649\t-9223372036854775809\t3.5e38\t1e309\n",
            "128\t32768\t2147483648\t9223372036854775808\t-3.5e38\t-1e309\n",
            "1.5\t2.5\t-0.5\t1.000001\t1.5\t-0\n",
            "1e2\t1e4\t2e9\t9.2e18\t1e-50\t5e-324\n",
            "\t\t\t\t\t\n",
            "inf\tnan\tx\t0x10\tinf\t-inf\n",
        );
        assert_eq!(
            rows(tsv.as_bytes()),
            [
                "{i8: -128, i16: -32768, i32: -2147483648, i64: -9223372036854775808, f: 3.4E38, d: 1.7E308}",
                "{i8: 127, i16: 32767, i32: 2147483647, i64: 9223372036854775807, f: -3.4E38, d: -1.7E308}",
                "{i8: null, i16: null, i32: null, i64: null, f: null, d: null}",
                "{i8: null, i16: null, i32: null, i64: null, f: null, d: null}",
                "{i8: null, i16: null, i32: null, i64: null, f: 1.5, d: -0E0}",
                "{i8: 100, i16: 10000, i32: 2000000000, i64: 9200000000000000000, f: 0E0, d: 5E-324}",
                "{i8: null, i16: null, i32: null, i64: null, f: null, d: null}",
                "{i8: null, i16: null, i32: null, i64: null, f: inf, d: -inf}",
            ]
        );
        // `inf`/`nan` spelled out are IEEE values; finite values that overflow are invalid.
        assert_eq!(parse_float("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_float("NaN").is_some_and(f64::is_nan));
        assert_eq!(parse_float("1e309"), None);
        assert_eq!(parse_f32("3.5e38"), None);
        assert_eq!(parse_f32("inf"), Some(f32::INFINITY));
        assert_eq!(parse_f32("1e-50"), Some(0.0));
    }
}
