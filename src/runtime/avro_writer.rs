//! Apache Avro output (`-o avro`).
//!
//! Records printed to stdout are written as TSV (as for `-o tsv`) and converted to an Avro Object
//! Container File by `AvroWriter`, as `-o parquet` does. The first record is the header: each
//! field is a column name, optionally followed by `:TYPE` (case-insensitive, e.g. `age:int`), and
//! columns without a type are strings. The header is split at its last colon, and the suffix is a
//! type only when it is a known type name: `url:http` is a column named `url:http`.
//!
//! The schema is a record named `Row` whose fields are all `["null", T]` unions, so that empty
//! values can be null. Types: `string`, `int`/`int32`, `long`/`int64`, `float`, `double`,
//! `boolean`, `date` (`int` + `date`), `timestamp` (`long` + `timestamp-micros`, UTC), `time`
//! (`long` + `time-micros`), `uuid` (`string` + `uuid`), `decimal(p,s)` (`bytes` + `decimal`;
//! `decimal` alone is `decimal(18,3)`) and `bytes` (written as hex text).
//!
//! Empty or unconvertible values are written as null, except for `string` (always written as is);
//! unconvertible values are counted in a warning. A field that is the `--null` text is null in
//! every column. Conversions are those of `-o parquet` (see `typed_value`); decimals are rounded
//! half away from zero to the scale, and are invalid when they need more digits than the
//! precision.
//!
//! Avro names must match `[A-Za-z_][A-Za-z0-9_]*`: other characters of a column name are replaced
//! with `_`, a name starting with a digit gets a leading `_`, a repeated name gets a `_2`, `_3`
//! suffix, and the original name is kept as the `doc` of the field, with a warning.
//!
//! A column of type `auto` (and, with `--infer-types`, a column without a type) gets its type from
//! the first `SAMPLE_SIZE` rows, which are buffered until the schema is known, as for `-o parquet`.
//!
//! The file is not compressed (codec `null`); it is written in blocks of about 64KB as the rows
//! come, and the last block when stdout is closed, at the end of the program.

use std::collections::HashSet;
use std::io::{self, Write};

use apache_avro::types::Value;
use apache_avro::{Schema, Writer};

use super::command::CommandEnv;
use super::typed_value::{
    Inferred, infer_type, parse_date, parse_f32, parse_float, parse_int, parse_parquet_bool,
    parse_time, parse_timestamp, unescape_tsv,
};
use super::writers::FileFactory;
use crate::common::FileSpec;

/// The number of rows buffered to infer the types of `auto` columns.
const SAMPLE_SIZE: usize = 64 * 1024;

/// The size of an Avro block, in bytes before compression.
const BLOCK_SIZE: usize = 64 * 1024;

/// Wraps a factory so that stdout is written as an Avro file (`-o avro`).
///
/// With `--infer-types` (the second field), columns without a `:TYPE` suffix are `:auto`.
#[derive(Clone)]
pub struct AvroFactory<F>(pub F, pub bool);

impl<F: FileFactory> FileFactory for AvroFactory<F> {
    type Output = F::Output;
    type Stdout = AvroWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<std::process::Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &std::path::Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        AvroWriter::new(self.0.stdout(), self.1)
    }
}

/// The type of a column, from the `:TYPE` suffix of its name in the header.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    String,
    Int,
    Long,
    Float,
    Double,
    Bool,
    Date,
    Timestamp,
    Time,
    Uuid,
    Decimal(u32, u32),
    Bytes,
}

/// The type of a column in the header: a known type, `auto` (`None`), or not a type at all.
enum Suffix {
    Type(Option<Kind>),
    NotAType,
}

impl Kind {
    fn parse(name: &str) -> io::Result<Suffix> {
        let lower = name.trim().to_ascii_lowercase();
        let kind = match lower.as_str() {
            "auto" => return Ok(Suffix::Type(None)),
            "string" | "str" | "utf8" | "text" | "varchar" => Kind::String,
            "int" | "int32" | "integer" => Kind::Int,
            "long" | "int64" | "bigint" => Kind::Long,
            "float" | "float32" | "real" => Kind::Float,
            "double" | "float64" => Kind::Double,
            "boolean" | "bool" => Kind::Bool,
            "date" => Kind::Date,
            "timestamp" | "datetime" => Kind::Timestamp,
            "time" => Kind::Time,
            "uuid" => Kind::Uuid,
            "bytes" | "binary" => Kind::Bytes,
            "decimal" => Kind::Decimal(18, 3),
            _ => match lower.strip_prefix("decimal") {
                Some(args) if args.trim_start().starts_with('(') => decimal_kind(name, args)?,
                _ => return Ok(Suffix::NotAType),
            },
        };
        Ok(Suffix::Type(Some(kind)))
    }

    /// The Avro schema of a value of this type, as JSON.
    fn schema(self) -> serde_json::Value {
        use serde_json::json;
        let logical = |base: &str, logical: &str| json!({"type": base, "logicalType": logical});
        match self {
            Kind::String => json!("string"),
            Kind::Int => json!("int"),
            Kind::Long => json!("long"),
            Kind::Float => json!("float"),
            Kind::Double => json!("double"),
            Kind::Bool => json!("boolean"),
            Kind::Bytes => json!("bytes"),
            Kind::Date => logical("int", "date"),
            Kind::Timestamp => logical("long", "timestamp-micros"),
            Kind::Time => logical("long", "time-micros"),
            Kind::Uuid => logical("string", "uuid"),
            Kind::Decimal(precision, scale) => json!({
                "type": "bytes",
                "logicalType": "decimal",
                "precision": precision,
                "scale": scale,
            }),
        }
    }
}

/// `decimal(p)` or `decimal(p,s)`, `args` being the text after `decimal`.
fn decimal_kind(name: &str, args: &str) -> io::Result<Kind> {
    let invalid = |why: &str| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid avro column type {:?}: {}", name.trim(), why),
        )
    };
    let inner = args
        .trim()
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| invalid("expected decimal(PRECISION,SCALE)"))?;
    let mut parts = inner.split(',').map(|p| p.trim().parse::<u32>());
    let precision = match parts.next() {
        Some(Ok(p)) => p,
        _ => return Err(invalid("expected decimal(PRECISION,SCALE)")),
    };
    let scale = match parts.next() {
        None => 0,
        Some(Ok(s)) => s,
        Some(Err(_)) => return Err(invalid("expected decimal(PRECISION,SCALE)")),
    };
    if parts.next().is_some() {
        return Err(invalid("expected decimal(PRECISION,SCALE)"));
    }
    if precision == 0 || precision > 38 {
        return Err(invalid("the precision must be between 1 and 38"));
    }
    if scale > precision {
        return Err(invalid("the scale must not exceed the precision"));
    }
    Ok(Kind::Decimal(precision, scale))
}

impl From<Inferred> for Kind {
    fn from(t: Inferred) -> Kind {
        match t {
            Inferred::String => Kind::String,
            Inferred::Int64 => Kind::Long,
            Inferred::Double => Kind::Double,
            Inferred::Bool => Kind::Bool,
            Inferred::Date => Kind::Date,
        }
    }
}

/// A column of the header: its name, and its type (`None` for `auto`).
type ColumnSpec = (String, Option<Kind>);

/// Parses a header field: `name` or `name:TYPE`; a column without a type is `auto` if `infer`.
fn parse_column(index: usize, raw: &[u8], infer: bool) -> io::Result<ColumnSpec> {
    let text = unescape_tsv(raw);
    let default = if infer { None } else { Some(Kind::String) };
    let (name, kind) = match text.rsplit_once(':') {
        Some((name, ty)) => match Kind::parse(ty)? {
            Suffix::Type(kind) => (name, kind),
            Suffix::NotAType => (&*text, default),
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

/// `name` as a valid Avro name: `[A-Za-z_][A-Za-z0-9_]*`.
fn avro_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// The Avro names of the columns: valid and unique (`a`, `a_2`, `a_3`). A name that is valid and
/// unique as given is kept, so that a column renamed to be valid does not take it.
fn field_names(names: &[String]) -> Vec<String> {
    let valid: Vec<Option<String>> = names
        .iter()
        .map(|name| Some(avro_name(name)).filter(|n| n == name))
        .collect();
    // Taken by the valid names; of duplicates, the first is kept and the others renamed.
    let mut used: HashSet<String> = valid.iter().flatten().cloned().collect();
    let mut seen = HashSet::new();
    names
        .iter()
        .zip(&valid)
        .map(|(name, valid)| {
            if let Some(v) = valid
                && seen.insert(v.clone()) {
                    return v.clone();
                }
            let base = avro_name(name);
            let mut candidate = base.clone();
            let mut n = 2;
            while used.contains(&candidate) {
                candidate = format!("{}_{}", base, n);
                n += 1;
            }
            used.insert(candidate.clone());
            candidate
        })
        .collect()
}

struct Column {
    /// The name in the header.
    name: String,
    kind: Kind,
    /// The number of non-empty values that could not be converted and were written as null.
    invalid: usize,
    /// The number of rows the type was inferred from, for an `auto` column.
    inferred_from: Option<usize>,
}

impl Column {
    /// The value of a field, given as TSV-escaped text: a union of null and the column type.
    fn value(&mut self, raw: &[u8]) -> Value {
        let null = Value::Union(0, Box::new(Value::Null));
        if crate::runtime::str_escape::is_tsv_null(raw) {
            return null;
        }
        let text = unescape_tsv(raw);
        // Empty values of an inferred column are null; a string column keeps empty and blank
        // values, other types treat blank values as empty.
        if self.kind == Kind::String {
            if self.inferred_from.is_some() && text.is_empty() {
                return null;
            }
            return Value::Union(1, Box::new(Value::String(text.into_owned())));
        }
        if text.trim().is_empty() {
            return null;
        }
        let value = match self.kind {
            Kind::String => unreachable!(),
            Kind::Int => parse_int(&text)
                .and_then(|i| i32::try_from(i).ok())
                .map(Value::Int),
            Kind::Long => parse_int(&text).map(Value::Long),
            Kind::Float => parse_f32(&text).map(Value::Float),
            Kind::Double => parse_float(&text).map(Value::Double),
            Kind::Bool => parse_parquet_bool(&text).map(Value::Boolean),
            Kind::Date => parse_date(&text).map(Value::Date),
            Kind::Timestamp => parse_timestamp(&text).map(Value::TimestampMicros),
            Kind::Time => parse_time(&text).map(Value::TimeMicros),
            Kind::Uuid => uuid::Uuid::parse_str(text.trim()).ok().map(Value::Uuid),
            Kind::Decimal(precision, scale) => decimal_bytes(&text, precision, scale)
                .map(|b| Value::Decimal(apache_avro::Decimal::from(b))),
            Kind::Bytes => parse_hex(&text).map(Value::Bytes),
        };
        match value {
            Some(v) => Value::Union(1, Box::new(v)),
            None => {
                self.invalid += 1;
                null
            }
        }
    }
}

/// The unscaled value of decimal text at `scale`, rounded half away from zero; `None` if it is
/// not a number or needs more than `precision` (at most 38) digits. The text is read as by the
/// `dec_*` functions (sign, thousands separators, fraction, exponent), but into an `i128`, so
/// that the precision is not limited to the 28 digits of `rust_decimal`.
fn unscaled_decimal(text: &str, precision: u32, scale: u32) -> Option<i128> {
    let t = text.trim();
    let (neg, body) = match t.as_bytes().first()? {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(i) => {
            let e = &body[i + 1..];
            let digits = e.strip_prefix(['+', '-']).unwrap_or(e);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            // A longer exponent cannot give a value of at most 38 digits, unless it is zero.
            let e: i64 = if digits.len() > 6 {
                if e.starts_with('-') {
                    -1_000_000
                } else {
                    1_000_000
                }
            } else {
                e.parse().ok()?
            };
            (&body[..i], e)
        }
        None => (body, 0),
    };
    let (int_part, frac) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };
    let int_digits = crate::runtime::decimal::ungroup(int_part)?;
    if (int_digits.is_empty() && frac.is_empty())
        || !int_digits.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // value = digits * 10^shift
    let mut digits: Vec<u8> = int_digits.bytes().chain(frac.bytes()).collect();
    let shift = exp + scale as i64 - frac.len() as i64;
    let first = digits
        .iter()
        .position(|&d| d != b'0')
        .unwrap_or(digits.len());
    digits.drain(..first);
    if digits.is_empty() {
        return Some(0);
    }
    let round_up = if shift >= 0 {
        if digits.len() as i64 + shift > precision as i64 {
            return None;
        }
        digits.extend(std::iter::repeat_n(b'0', shift as usize));
        false
    } else {
        let drop = (-shift) as usize;
        if drop > digits.len() {
            digits.clear();
            false
        } else {
            let up = digits[digits.len() - drop] >= b'5';
            digits.truncate(digits.len() - drop);
            up
        }
    };
    if digits.len() > precision as usize {
        return None;
    }
    let mut n: i128 = 0;
    for d in digits {
        n = n * 10 + (d - b'0') as i128;
    }
    if round_up {
        n += 1;
        if n.to_string().len() > precision as usize {
            return None;
        }
    }
    Some(if neg { -n } else { n })
}

/// The unscaled value of a decimal, as the minimal big-endian two's complement bytes: the text
/// rounded half away from zero to `scale` digits; `None` if it is not a number or needs more than
/// `precision` digits.
fn decimal_bytes(text: &str, precision: u32, scale: u32) -> Option<Vec<u8>> {
    let unscaled = unscaled_decimal(text, precision, scale)?;
    let bytes = unscaled.to_be_bytes();
    // drop the leading bytes that only repeat the sign
    let mut start = 0;
    while start < bytes.len() - 1 {
        let (b, next) = (bytes[start], bytes[start + 1]);
        if (b == 0x00 && next & 0x80 == 0) || (b == 0xff && next & 0x80 != 0) {
            start += 1;
        } else {
            break;
        }
    }
    Some(bytes[start..].to_vec())
}

/// Bytes from hex text (`dead`, `0xDEAD`); `None` if it is not hex of an even length.
fn parse_hex(text: &str) -> Option<Vec<u8>> {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    if !t.len().is_multiple_of(2) || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).ok())
        .collect()
}

fn to_io(e: apache_avro::Error) -> io::Error {
    io::Error::other(format!("failed to write avro: {}", e))
}

struct Encoder {
    columns: Vec<Column>,
    field_names: Vec<String>,
    /// Writes into memory; the bytes are moved to the output as blocks are completed.
    writer: Writer<'static, Vec<u8>>,
}

impl Encoder {
    /// Creates the encoder for the header `specs`; the types of `auto` columns are inferred from
    /// `sample`, the first rows, which the caller then pushes.
    fn new(specs: Vec<ColumnSpec>, sample: &[Vec<u8>]) -> io::Result<Encoder> {
        let columns: Vec<Column> = specs
            .into_iter()
            .enumerate()
            .map(|(i, (name, kind))| match kind {
                Some(kind) => Column {
                    name,
                    kind,
                    invalid: 0,
                    inferred_from: None,
                },
                None => Column {
                    name,
                    kind: infer_type(sample, i).into(),
                    invalid: 0,
                    inferred_from: Some(sample.len()),
                },
            })
            .collect();
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let field_names = field_names(&names);
        let fields: Vec<serde_json::Value> = columns
            .iter()
            .zip(&field_names)
            .map(|(c, field)| {
                let mut f = serde_json::json!({
                    "name": field,
                    "type": ["null", c.kind.schema()],
                    "default": null,
                });
                if *field != c.name {
                    f["doc"] = serde_json::Value::String(c.name.clone());
                    crate::runtime::stdlib_notice(
                        "avro",
                        format!(
                            "column {:?} is written as the Avro field {:?} (Avro names are \
                             [A-Za-z_][A-Za-z0-9_]* and unique)",
                            c.name, field
                        ),
                    );
                }
                f
            })
            .collect();
        let schema = serde_json::json!({
            "type": "record",
            "name": "Row",
            "fields": fields,
        });
        let schema = Schema::parse_str(&schema.to_string()).map_err(to_io)?;
        // The writer borrows its schema: there is one per program, so it lives until the end.
        let schema: &'static Schema = Box::leak(Box::new(schema));
        let writer = Writer::builder()
            .schema(schema)
            .writer(Vec::new())
            .block_size(BLOCK_SIZE)
            .build()
            .map_err(to_io)?;
        Ok(Encoder {
            columns,
            field_names,
            writer,
        })
    }

    fn push_row(&mut self, line: &[u8], out: &mut impl Write) -> io::Result<()> {
        let mut cells = line.split(|b| *b == b'\t');
        let record: Vec<(String, Value)> = self
            .columns
            .iter_mut()
            .zip(&self.field_names)
            // Missing trailing fields are empty; extra fields are ignored.
            .map(|(col, name)| (name.clone(), col.value(cells.next().unwrap_or(b""))))
            .collect();
        self.writer
            .append_value_ref(&Value::Record(record))
            .map_err(to_io)?;
        // A block is written to the inner buffer when it is full: move it to the output.
        let buf = self.writer.get_mut();
        if !buf.is_empty() {
            out.write_all(buf)?;
            buf.clear();
        }
        Ok(())
    }

    fn finish(mut self, out: &mut impl Write) -> io::Result<()> {
        // Writes the header (also for a file without rows) and the last block.
        self.writer.flush().map_err(to_io)?;
        let bytes = std::mem::take(self.writer.get_mut());
        out.write_all(&bytes)?;
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
                    "zawk: avro column {:?} ({:?}{}): {} invalid value(s) written as null",
                    col.name, col.kind, inferred, col.invalid
                );
            }
        }
        out.flush()
    }
}

/// Converts TSV records written to it into an Avro file written to `inner`; see the module
/// documentation.
pub struct AvroWriter<W: Write> {
    inner: W,
    /// The current, not yet newline-terminated, record.
    pending: Vec<u8>,
    /// Created from the first record (the header), or after the sample with `auto` columns.
    encoder: Option<Encoder>,
    /// The header while the rows that `auto` column types are inferred from are buffered.
    header: Option<Vec<ColumnSpec>>,
    /// The buffered rows, while `header` is set.
    sample: Vec<Vec<u8>>,
    /// Whether columns without a type are `auto` (`--infer-types`).
    infer_types: bool,
    /// Set after an error, which has already been reported; later records are dropped.
    failed: bool,
}

impl<W: Write> AvroWriter<W> {
    pub fn new(inner: W, infer_types: bool) -> AvroWriter<W> {
        AvroWriter {
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
            enc.push_row(row, &mut self.inner)?;
        }
        self.encoder = Some(enc);
        Ok(())
    }

    fn write_record(&mut self, line: &[u8]) -> io::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(specs) = &self.header {
            self.sample.push(line.to_vec());
            if self.sample.len() >= SAMPLE_SIZE {
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
                // program silently), so it is reported here. The writer thread must not exit the
                // process itself: the error stops this writer, and the main thread then ends the
                // program, flushing the other outputs, with exit code 2 (see `exit_code`).
                Err(e) => {
                    if e.kind() == io::ErrorKind::InvalidInput {
                        crate::runtime::writers::report_fatal_output_error(&e);
                    }
                    return Err(e);
                }
            },
            Some(enc) => enc.push_row(line, &mut self.inner)?,
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

    /// Writes the remaining rows and the last block.
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

impl<W: Write> Write for AvroWriter<W> {
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
        // Blocks are written as they are completed.
        Ok(())
    }
}

impl<W: Write> Drop for AvroWriter<W> {
    fn drop(&mut self) {
        if let Err(e) = self.finish() {
            crate::runtime::writers::report_stdout_render_error(&e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apache_avro::Reader;

    fn write(data: &[u8], infer: bool) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut w = AvroWriter::new(&mut out, infer);
            w.write_all(data).unwrap();
        }
        out
    }

    fn records(data: &[u8], infer: bool) -> (Schema, Vec<Value>) {
        let out = write(data, infer);
        let reader = Reader::new(&out[..]).unwrap();
        let schema = reader.writer_schema().clone();
        let values = reader.map(|v| v.unwrap()).collect();
        (schema, values)
    }

    /// The values of a record, without the unions.
    fn fields(v: &Value) -> Vec<Value> {
        match v {
            Value::Record(fields) => fields
                .iter()
                .map(|(_, v)| match v {
                    Value::Union(_, v) => (**v).clone(),
                    v => v.clone(),
                })
                .collect(),
            _ => panic!("not a record: {:?}", v),
        }
    }

    #[test]
    fn typed_columns() {
        let (schema, rows) = records(
            b"name\tage:int\tn:LONG\tok:Boolean\td:DATE\tts:timestamp\tt:Time\tu:UUID\tprice:decimal(10,2)\traw:bytes\tf:float\tx:double\n\
              Alice\t30\t1e3\ttrue\t2024-01-02\t2024-01-02 03:04:05\t03:04:05\t67e55044-10b1-426f-9247-bb680e5fe0c8\t12.345\tDEAD\t1.5\t-2\n\
              Bob\tx\t1.5\tmaybe\tnope\t1700000000000\t3600\tnot-uuid\t123456789.5\tabc\tx\tinf\n\
              \t\t\t\t\t\t\t\t\t\t\t\n",
            false,
        );
        assert!(schema.canonical_form().contains("\"name\":\"Row\""));
        assert_eq!(rows.len(), 3);
        let r0 = fields(&rows[0]);
        assert_eq!(r0[0], Value::String("Alice".into()));
        assert_eq!(r0[1], Value::Int(30));
        assert_eq!(r0[2], Value::Long(1000));
        assert_eq!(r0[3], Value::Boolean(true));
        assert_eq!(r0[4], Value::Date(19724));
        assert_eq!(r0[5], Value::TimestampMicros(1_704_164_645_000_000));
        assert_eq!(r0[6], Value::TimeMicros(11_045_000_000));
        assert!(matches!(r0[7], Value::Uuid(_)));
        // 12.345 rounded half away from zero to 12.35
        assert_eq!(
            r0[8],
            Value::Decimal(apache_avro::Decimal::from(vec![0x04, 0xd3]))
        );
        assert_eq!(r0[9], Value::Bytes(vec![0xde, 0xad]));
        assert_eq!(r0[10], Value::Float(1.5));
        assert_eq!(r0[11], Value::Double(-2.0));
        let r1 = fields(&rows[1]);
        assert_eq!(r1[0], Value::String("Bob".into()));
        assert!(r1[1..5].iter().all(|v| *v == Value::Null), "{:?}", r1);
        assert_eq!(r1[5], Value::TimestampMicros(1_700_000_000_000_000));
        assert_eq!(r1[6], Value::TimeMicros(3_600_000_000));
        assert!(r1[7..11].iter().all(|v| *v == Value::Null), "{:?}", r1);
        assert_eq!(r1[11], Value::Double(f64::INFINITY));
        let r2 = fields(&rows[2]);
        assert_eq!(r2[0], Value::String(String::new()));
        assert!(r2[1..].iter().all(|v| *v == Value::Null), "{:?}", r2);
    }

    #[test]
    fn invalid_values_are_counted() {
        let mut out = Vec::new();
        let mut w = AvroWriter::new(&mut out, false);
        w.write_all(b"ok:boolean\nYes\n0.0\nmaybe\n\n").unwrap();
        assert_eq!(w.encoder.as_ref().unwrap().columns[0].invalid, 1);
    }

    #[test]
    fn names_and_suffixes() {
        let (schema, rows) = records(
            b"first name\turl:http\t1st\ta-b\ta_b\t\n1\t2\t3\t4\t5\t6\n",
            false,
        );
        let Schema::Record(r) = schema else {
            panic!("not a record")
        };
        let names: Vec<&str> = r.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["first_name", "url_http", "_1st", "a_b_2", "a_b", "c6"]
        );
        assert_eq!(r.fields[0].doc.as_deref(), Some("first name"));
        assert_eq!(r.fields[3].doc.as_deref(), Some("a-b"));
        assert_eq!(r.fields[4].doc, None);
        assert_eq!(r.fields[5].doc, None);
        assert_eq!(fields(&rows[0])[1], Value::String("2".into()));
    }

    #[test]
    fn auto_columns() {
        let (schema, rows) = records(
            b"id\tname\tscore\tok\td\tzip\n1\tJackie\t1.5\ttrue\t2024-01-02\t007\n2\t\t2\tFALSE\t2024-01-03\t123\n",
            true,
        );
        let form = schema.canonical_form();
        for t in [
            r#"{"name":"id","type":["null","long"]}"#,
            r#"{"name":"score","type":["null","double"]}"#,
            r#"{"name":"ok","type":["null","boolean"]}"#,
            r#"{"name":"zip","type":["null","string"]}"#,
        ] {
            assert!(form.contains(t), "{}", form);
        }
        assert_eq!(
            fields(&rows[1]),
            [
                Value::Long(2),
                Value::Null,
                Value::Double(2.0),
                Value::Boolean(false),
                Value::Date(19725),
                Value::String("123".into())
            ]
        );
        // only a header: a valid file without records
        let (_, rows) = records(b"n:auto\n", false);
        assert!(rows.is_empty());
    }

    #[test]
    fn decimals() {
        assert_eq!(decimal_bytes("-0.5", 5, 2), Some(vec![0xce]));
        assert_eq!(decimal_bytes("128", 3, 0), Some(vec![0x00, 0x80]));
        assert_eq!(decimal_bytes("-128", 3, 0), Some(vec![0x80]));
        assert_eq!(decimal_bytes("0", 3, 0), Some(vec![0x00]));
        assert_eq!(decimal_bytes("1000", 3, 0), None);
        assert_eq!(decimal_bytes("abc", 3, 0), None);
        // beyond the 28 digits of rust_decimal
        let max38 = "9".repeat(38);
        assert_eq!(
            unscaled_decimal(&max38, 38, 0),
            Some(max38.parse::<i128>().unwrap())
        );
        assert_eq!(unscaled_decimal(&format!("{}0", max38), 38, 0), None);
        assert_eq!(
            unscaled_decimal("-12345678901234567890123456789012.3456", 38, 4),
            Some(-123456789012345678901234567890123456)
        );
        assert_eq!(unscaled_decimal("1.005", 5, 2), Some(101));
        assert_eq!(unscaled_decimal("-1.005", 5, 2), Some(-101));
        assert_eq!(unscaled_decimal("0.004", 5, 2), Some(0));
        assert_eq!(unscaled_decimal("0.5", 3, 0), Some(1));
        assert_eq!(unscaled_decimal("9.995", 3, 2), None);
        assert_eq!(unscaled_decimal("1,234.5", 6, 1), Some(12345));
        assert_eq!(unscaled_decimal("1.5e3", 6, 1), Some(15000));
        assert_eq!(unscaled_decimal("15e-1", 3, 0), Some(2));
        assert_eq!(unscaled_decimal("0e99999999", 3, 0), Some(0));
        assert_eq!(unscaled_decimal("1e99999999", 38, 0), None);
        assert_eq!(unscaled_decimal("1e-99999999", 38, 0), Some(0));
        assert_eq!(unscaled_decimal("1.2.3", 5, 0), None);
        assert_eq!(unscaled_decimal("1e", 5, 0), None);
        assert_eq!(unscaled_decimal(".", 5, 0), None);
        assert!(Kind::parse("decimal(38,38)").is_ok());
        assert!(matches!(
            Kind::parse("DECIMAL(38, 4)").unwrap(),
            Suffix::Type(Some(Kind::Decimal(38, 4)))
        ));
        assert!(Kind::parse("decimal(2,3)").is_err());
        assert!(Kind::parse("decimal(x)").is_err());
        assert_eq!(parse_hex("0xDEAD"), Some(vec![0xde, 0xad]));
        assert_eq!(parse_hex("abc"), None);
    }

    #[test]
    fn valid_names_are_kept() {
        let names = |v: &[&str]| field_names(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(names(&["a b", "a_b"]), ["a_b_2", "a_b"]);
        assert_eq!(names(&["a", "a", "a_2"]), ["a", "a_3", "a_2"]);
        assert_eq!(names(&["1x", "_1x", "x"]), ["_1x_2", "_1x", "x"]);
    }

    #[test]
    fn invalid_decimal_header() {
        let mut w = AvroWriter::new(Vec::new(), false);
        assert!(w.write_all(b"a:decimal(0,0)\n").is_err());
    }
}
