//! Apache Avro (`-i avro`) input support.
//!
//! An Avro Object Container File is converted to JSON Lines, one JSON object per record, which is
//! then read like `-i jsonl` input (as Parquet input is): `$1..$NF` are the fields of the
//! top-level record in schema order, `FI` maps each field name to its index, and `$0` is the
//! record as a JSON object.
//!
//! Records are read as `apache_avro::types::Value`s, without serde or Arrow. Value mapping:
//! numbers are plain text (NaN and infinities as for Parquet), booleans are `1`/`0`, strings and
//! enum symbols are as is, a union is its actual branch, `date` is `YYYY-MM-DD`, `time-*` is
//! `HH:MM:SS[.f]`, `timestamp-*` is RFC 3339 in UTC (`2024-01-15T10:30:00Z`), `local-timestamp-*`
//! is `YYYY-MM-DD HH:MM:SS[.f]`, decimals are exact decimal text, UUIDs are in the standard form,
//! `bytes` and `fixed` are hex, records, arrays and maps are JSON text, and null is empty (or the
//! `--null` text).
//!
//! The schema is in the header of the file, so standard input, S3 objects and HTTP(S) URLs are
//! streamed. All codecs of the specification (null, deflate, snappy, zstandard, bzip2, xz) are
//! detected from the header.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read};

use apache_avro::schema::{Name, RecordField, ResolvedSchema, UnionSchema};
use apache_avro::types::Value;
use apache_avro::{Reader, Schema};

use super::parquet::{
    Kind, date, float_text, hex, push_json, push_json_str, time_nanos, timestamp_nanos,
};
use crate::inspect::Cell;

/// The magic bytes at the start of an Avro Object Container File.
const MAGIC: &[u8; 4] = b"Obj\x01";

/// Whether `file` names an Avro file: it ends with `.avro` and, when it is a local file, starts
/// with the `Obj\x01` magic. Remote objects (S3, HTTP) are told by their extension only.
pub fn is_avro_file(file: &str) -> bool {
    let name = file.split('?').next().unwrap_or_default();
    if !name.to_ascii_lowercase().ends_with(".avro") {
        return false;
    }
    let path = std::path::Path::new(file);
    if !path.is_file() {
        return true;
    }
    let mut magic = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map(|_| &magic == MAGIC)
        .unwrap_or(true)
}

enum Source {
    File(String),
    Stdin,
    /// An already opened input (named for messages).
    Reader(Option<Box<dyn Read + Send>>, String),
}

type AvroReader = Reader<'static, Box<dyn Read + Send>>;

/// The named types (records, enums, fixed) of a schema by their full names: a named type that is
/// used again after its definition is a `Schema::Ref` to it.
type Names = HashMap<Name, Schema>;

/// The named types defined in `schema` (empty when they cannot be resolved).
fn named_types(schema: &Schema) -> Names {
    ResolvedSchema::try_from(schema)
        .map(|r| {
            r.get_names()
                .iter()
                .map(|(name, schema)| (name.clone(), (*schema).clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// The schema itself, or the definition of the named type it refers to.
fn resolve<'a>(names: &'a Names, schema: Option<&'a Schema>) -> Option<&'a Schema> {
    match schema {
        Some(Schema::Ref { name }) => names.get(name),
        s => s,
    }
}

struct Rows {
    reader: AvroReader,
    /// The top-level fields: their names and schemas (empty when the file schema is not a record:
    /// each value is then a record with a single field `value`).
    fields: Vec<(String, Schema)>,
    record: bool,
    names: Names,
}

/// The records of an Avro file (or of standard input) as JSON Lines.
pub struct AvroJson {
    source: Source,
    rows: Option<Rows>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
}

impl AvroJson {
    pub fn file(path: String) -> AvroJson {
        AvroJson::new(Source::File(path))
    }
    pub fn stdin() -> AvroJson {
        AvroJson::new(Source::Stdin)
    }
    /// The records of an opened input; `name` is used in messages.
    pub fn reader(input: Box<dyn Read + Send>, name: &str) -> AvroJson {
        AvroJson::new(Source::Reader(Some(input), name.to_string()))
    }
    fn new(source: Source) -> AvroJson {
        AvroJson {
            source,
            rows: None,
            buf: Vec::new(),
            pos: 0,
            done: false,
        }
    }

    /// Check up front that a local `path` is an Avro file that can be read (its header and
    /// schema), so that problems are reported clearly before any input is processed. Remote
    /// objects are checked when they are read.
    pub fn check(path: &str) -> std::result::Result<(), String> {
        if crate::runtime::s3::is_s3_url(path) || crate::runtime::http::is_http_url(path) {
            return Ok(());
        }
        let file = File::open(path).map_err(|e| {
            format!(
                "zawk: {}{}",
                crate::common::FATAL_PREFIX,
                crate::common::InputOpenError::new_io(path, &e)
            )
        })?;
        let reader: Box<dyn Read + Send> = Box::new(BufReader::new(file));
        Reader::new(reader)
            .map(|_| ())
            .map_err(|e| format!("cannot read avro file `{}': {}", path, e))
    }

    fn name(&self) -> &str {
        match &self.source {
            Source::File(path) | Source::Reader(_, path) => path,
            Source::Stdin => "-",
        }
    }

    fn error(&self, e: impl std::fmt::Display) -> io::Error {
        io::Error::other(format!("cannot read avro file `{}': {}", self.name(), e))
    }

    fn open(&mut self) -> io::Result<Rows> {
        if let Source::Reader(input, _) = &mut self.source {
            let input = input.take().ok_or_else(|| self.error("already read"))?;
            return self.open_reader(input);
        }
        let input: Box<dyn Read + Send> = match &self.source {
            Source::File(path) if crate::runtime::s3::is_s3_url(path) => Box::new(BufReader::new(
                crate::runtime::s3::S3Reader::new(path.clone()),
            )),
            Source::File(path) if crate::runtime::http::is_http_url(path) => Box::new(
                BufReader::new(crate::runtime::http::open(path).map_err(|e| self.error(e))?),
            ),
            Source::File(path) => {
                Box::new(BufReader::new(File::open(path).map_err(|e| self.error(e))?))
            }
            Source::Stdin => Box::new(BufReader::new(io::stdin())),
            Source::Reader(..) => unreachable!(),
        };
        self.open_reader(input)
    }

    fn open_reader(&self, input: Box<dyn Read + Send>) -> io::Result<Rows> {
        let reader = Reader::new(input).map_err(|e| self.error(e))?;
        let names = named_types(reader.writer_schema());
        let (fields, record) = match reader.writer_schema() {
            Schema::Record(r) => (
                r.fields
                    .iter()
                    .map(|f: &RecordField| (f.name.clone(), f.schema.clone()))
                    .collect(),
                true,
            ),
            other => (vec![(String::from("value"), other.clone())], false),
        };
        Ok(Rows {
            reader,
            fields,
            record,
            names,
        })
    }

    /// Append the next records (about 64KB) to the buffer as JSON Lines.
    fn fill(&mut self) -> io::Result<()> {
        if self.rows.is_none() {
            self.rows = Some(self.open()?);
        }
        self.buf.clear();
        self.pos = 0;
        let mut line = String::new();
        while self.buf.len() < 64 << 10 {
            let rows = self.rows.as_mut().unwrap();
            let value = match rows.reader.next() {
                Some(Ok(value)) => value,
                Some(Err(e)) => return Err(self.error(e)),
                None => {
                    self.done = true;
                    break;
                }
            };
            line.clear();
            line.push('{');
            let names = &rows.names;
            for (i, (name, value, schema)) in rows.columns(value).enumerate() {
                if i > 0 {
                    line.push(',');
                }
                push_json_str(&mut line, name);
                line.push(':');
                let (text, kind) = value_text(names, &value, Some(schema));
                push_json(&mut line, &text, kind);
            }
            line.push_str("}\n");
            self.buf.extend_from_slice(line.as_bytes());
        }
        Ok(())
    }

    pub fn table(mut self) -> io::Result<AvroTable> {
        let rows = self.open()?;
        Ok(AvroTable {
            rows,
            name: self.name().to_string(),
        })
    }
}

impl Rows {
    /// The top-level fields of a record: name, value (null when missing) and schema.
    fn columns(&self, value: Value) -> impl Iterator<Item = (&str, Value, &Schema)> {
        let mut values: Vec<Value> = if self.record {
            match value {
                Value::Record(fields) => fields.into_iter().map(|(_, v)| v).collect(),
                _ => Vec::new(),
            }
        } else {
            vec![value]
        };
        values.resize(self.fields.len(), Value::Null);
        self.fields
            .iter()
            .zip(values)
            .map(|((name, schema), value)| (name.as_str(), value, schema))
    }
}

impl Read for AvroJson {
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

/// The records of an Avro file as cells, for `zawk inspect`.
pub struct AvroTable {
    rows: Rows,
    name: String,
}

impl AvroTable {
    /// The top-level field names and their types (`LONG`, `STRING`, `DECIMAL(10,2)`, ...).
    pub fn columns(&self) -> Vec<(String, String)> {
        self.rows
            .fields
            .iter()
            .map(|(name, schema)| (name.clone(), type_name(&self.rows.names, schema)))
            .collect()
    }

    pub fn next_row(&mut self) -> Option<io::Result<Vec<Cell>>> {
        let value = match self.rows.reader.next()? {
            Ok(value) => value,
            Err(e) => {
                let msg = format!("cannot read avro file `{}': {}", self.name, e);
                return Some(Err(io::Error::other(msg)));
            }
        };
        let names = &self.rows.names;
        let cells = self
            .rows
            .columns(value)
            .map(|(_, value, schema)| value_cell(names, &value, schema))
            .collect();
        Some(Ok(cells))
    }
}

fn value_cell(names: &Names, value: &Value, schema: &Schema) -> Cell {
    let (value, schema) = match value {
        Value::Union(index, v) => (v.as_ref(), branch(names, Some(schema), *index)),
        v => (v, Some(schema)),
    };
    let (text, kind) = value_text(names, value, schema);
    match value {
        Value::Null => Cell::Null,
        Value::Record(_) | Value::Array(_) | Value::Map(_) | Value::Duration(_) => {
            Cell::Nested(text)
        }
        _ => match kind {
            Kind::Bool => Cell::Bool(text == "1"),
            // NaN and infinities: null in JSON, but numbers to awk
            Kind::Null | Kind::Raw => Cell::Num(text),
            Kind::Str => Cell::Str(text),
        },
    }
}

/// The type of a top-level field, named as in `-o avro` headers where possible. A union of null
/// and one type is named after that type.
fn type_name(names: &Names, schema: &Schema) -> String {
    // an unknown name stays a `Ref`
    let schema = resolve(names, Some(schema)).unwrap_or(schema);
    let name = match schema {
        Schema::Union(u) => {
            let types: Vec<&Schema> = u
                .variants()
                .iter()
                .filter(|s| !matches!(s, Schema::Null))
                .collect();
            return match types.as_slice() {
                [one] => type_name(names, one),
                _ => "UNION".to_string(),
            };
        }
        Schema::Null => "NULL",
        Schema::Boolean => "BOOLEAN",
        Schema::Int => "INT",
        Schema::Long => "LONG",
        Schema::Float => "FLOAT",
        Schema::Double => "DOUBLE",
        Schema::Bytes => "BYTES",
        Schema::String => "STRING",
        Schema::Array(_) => "ARRAY",
        Schema::Map(_) => "MAP",
        Schema::Record(_) => "RECORD",
        Schema::Enum(_) => "ENUM",
        Schema::Fixed(f) => return format!("FIXED({})", f.size),
        Schema::Decimal(d) => return format!("DECIMAL({},{})", d.precision, d.scale),
        Schema::BigDecimal => "BIG-DECIMAL",
        Schema::Uuid(_) => "UUID",
        Schema::Date => "DATE",
        Schema::TimeMillis | Schema::TimeMicros => "TIME",
        Schema::TimestampMillis | Schema::TimestampMicros | Schema::TimestampNanos => "TIMESTAMP",
        Schema::LocalTimestampMillis
        | Schema::LocalTimestampMicros
        | Schema::LocalTimestampNanos => "LOCAL-TIMESTAMP",
        Schema::Duration(_) => "DURATION",
        Schema::Ref { name } => return name.name().to_string(),
    };
    name.to_string()
}

/// The schema of the branch `index` of a union schema.
fn branch<'a>(names: &'a Names, schema: Option<&'a Schema>, index: u32) -> Option<&'a Schema> {
    match resolve(names, schema) {
        Some(Schema::Union(u)) => resolve(names, UnionSchema::variants(u).get(index as usize)),
        _ => None,
    }
}

/// The awk text of a value, and how it is written in the JSON of `$0`. The schema, when known,
/// gives the scale of decimals.
fn value_text(names: &Names, value: &Value, schema: Option<&Schema>) -> (String, Kind) {
    let schema = resolve(names, schema);
    let int = |v: i128| (v.to_string(), Kind::Raw);
    let micros = |us: i64| us.saturating_mul(1_000);
    let millis = |ms: i64| ms.saturating_mul(1_000_000);
    match value {
        Value::Null => (String::new(), Kind::Null),
        Value::Boolean(b) => ((if *b { "1" } else { "0" }).to_string(), Kind::Bool),
        Value::Int(v) => int(*v as i128),
        Value::Long(v) => int(*v as i128),
        Value::Float(v) => float_text(*v as f64, Some(*v)),
        Value::Double(v) => float_text(*v, None),
        Value::Bytes(b) | Value::Fixed(_, b) => (hex(b), Kind::Str),
        Value::String(s) => (s.clone(), Kind::Str),
        Value::Enum(_, symbol) => (symbol.clone(), Kind::Str),
        Value::Union(index, v) => value_text(names, v, branch(names, schema, *index)),
        Value::Date(days) => (date(*days), Kind::Str),
        Value::Decimal(d) => {
            let scale = match schema {
                Some(Schema::Decimal(d)) => d.scale,
                _ => 0,
            };
            match Vec::<u8>::try_from(d) {
                Ok(bytes) => (decimal_text(&bytes, scale), Kind::Raw),
                Err(_) => (String::new(), Kind::Null),
            }
        }
        Value::BigDecimal(d) => (d.to_string(), Kind::Raw),
        Value::TimeMillis(ms) => (time_nanos(millis(*ms as i64)), Kind::Str),
        Value::TimeMicros(us) => (time_nanos(micros(*us)), Kind::Str),
        Value::TimestampMillis(ms) => (rfc3339(millis(*ms)), Kind::Str),
        Value::TimestampMicros(us) => (rfc3339(micros(*us)), Kind::Str),
        Value::TimestampNanos(ns) => (rfc3339(*ns), Kind::Str),
        Value::LocalTimestampMillis(ms) => (timestamp_nanos(millis(*ms)), Kind::Str),
        Value::LocalTimestampMicros(us) => (timestamp_nanos(micros(*us)), Kind::Str),
        Value::LocalTimestampNanos(ns) => (timestamp_nanos(*ns), Kind::Str),
        Value::Uuid(u) => (u.to_string(), Kind::Str),
        Value::Duration(d) => (
            format!(
                "{{\"months\":{},\"days\":{},\"millis\":{}}}",
                u32::from(d.months()),
                u32::from(d.days()),
                u32::from(d.millis())
            ),
            Kind::Raw,
        ),
        Value::Record(_) | Value::Array(_) | Value::Map(_) => {
            let mut json = String::new();
            write_json(names, &mut json, value, schema);
            (json, Kind::Raw)
        }
    }
}

/// Write a value as JSON: records and maps as objects, arrays as arrays, scalars as for `$0`.
fn write_json(names: &Names, out: &mut String, value: &Value, schema: Option<&Schema>) {
    let schema = resolve(names, schema);
    match value {
        Value::Union(index, v) => write_json(names, out, v, branch(names, schema, *index)),
        Value::Record(fields) => {
            let field_schemas = match schema {
                Some(Schema::Record(r)) => Some(&r.fields),
                _ => None,
            };
            out.push('{');
            for (i, (name, v)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_str(out, name);
                out.push(':');
                let s = field_schemas.and_then(|f| f.get(i)).map(|f| &f.schema);
                write_json(names, out, v, s);
            }
            out.push('}');
        }
        Value::Array(items) => {
            let s = match schema {
                Some(Schema::Array(a)) => Some(a.items.as_ref()),
                _ => None,
            };
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(names, out, v, s);
            }
            out.push(']');
        }
        Value::Map(entries) => {
            let s = match schema {
                Some(Schema::Map(m)) => Some(m.types.as_ref()),
                _ => None,
            };
            // HashMap order is random: sort the keys so that the text is stable.
            let mut keys: Vec<&String> = entries.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_str(out, key);
                out.push(':');
                write_json(names, out, &entries[key], s);
            }
            out.push('}');
        }
        _ => {
            let (text, kind) = value_text(names, value, schema);
            push_json(out, &text, kind);
        }
    }
}

/// `2024-01-15T10:30:00Z`, with the fraction of a second only when it is not zero.
fn rfc3339(nanos: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(nanos)
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
}

/// The exact text of the decimal with the big-endian two's complement unscaled value `bytes` and
/// `scale` digits after the point, e.g. `12345.67` or `-0.50`. Values of any size are exact.
fn decimal_text(bytes: &[u8], scale: usize) -> String {
    let negative = bytes.first().is_some_and(|b| b & 0x80 != 0);
    // the magnitude, big-endian
    let mut mag: Vec<u8> = bytes.to_vec();
    if negative {
        for b in mag.iter_mut() {
            *b = !*b;
        }
        for b in mag.iter_mut().rev() {
            let (v, carry) = b.overflowing_add(1);
            *b = v;
            if !carry {
                break;
            }
        }
    }
    // repeated division by 10
    let mut digits = Vec::new();
    while mag.iter().any(|b| *b != 0) {
        let mut rem: u32 = 0;
        for b in mag.iter_mut() {
            let cur = (rem << 8) | *b as u32;
            *b = (cur / 10) as u8;
            rem = cur % 10;
        }
        digits.push(b'0' + rem as u8);
    }
    if digits.is_empty() {
        digits.push(b'0');
    }
    digits.reverse();
    let digits = String::from_utf8(digits).unwrap();
    let sign = if negative { "-" } else { "" };
    if scale == 0 {
        return format!("{sign}{digits}");
    }
    let digits = format!("{:0>width$}", digits, width = scale + 1);
    let (int, frac) = digits.split_at(digits.len() - scale);
    format!("{sign}{int}.{frac}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use apache_avro::Writer;
    use apache_avro::types::Record;

    fn json_lines(data: Vec<u8>) -> String {
        let path = std::env::temp_dir().join(format!(
            "zawk-avro-test-{}-{:?}.avro",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, data).unwrap();
        let mut out = String::new();
        AvroJson::file(path.to_string_lossy().into_owned())
            .read_to_string(&mut out)
            .unwrap();
        let _ = std::fs::remove_file(&path);
        out
    }

    #[test]
    fn decimals_are_exact() {
        assert_eq!(decimal_text(&[0x30, 0x39], 2), "123.45");
        assert_eq!(decimal_text(&[0xff, 0xce], 2), "-0.50");
        assert_eq!(decimal_text(&[0x00], 3), "0.000");
        assert_eq!(decimal_text(&[0x80], 0), "-128");
        // more than 128 bits
        let mut big = vec![0x01];
        big.extend([0u8; 20]);
        assert_eq!(
            decimal_text(&big, 0),
            "1461501637330902918203684832716283019655932542976"
        );
    }

    #[test]
    fn values_to_json_lines() {
        let schema = Schema::parse_str(
            r#"{"type":"record","name":"Row","fields":[
                {"name":"id","type":"long"},
                {"name":"name","type":["null","string"]},
                {"name":"ok","type":"boolean"},
                {"name":"score","type":"double"},
                {"name":"color","type":{"type":"enum","name":"Color","symbols":["RED","GREEN"]}},
                {"name":"d","type":{"type":"int","logicalType":"date"}},
                {"name":"t","type":{"type":"long","logicalType":"time-micros"}},
                {"name":"ts","type":{"type":"long","logicalType":"timestamp-millis"}},
                {"name":"lts","type":{"type":"long","logicalType":"local-timestamp-micros"}},
                {"name":"price","type":["null",{"type":"bytes","logicalType":"decimal","precision":10,"scale":2}]},
                {"name":"u","type":{"type":"string","logicalType":"uuid"}},
                {"name":"raw","type":"bytes"},
                {"name":"tags","type":{"type":"array","items":"string"}},
                {"name":"attrs","type":{"type":"map","values":"long"}},
                {"name":"inner","type":{"type":"record","name":"Inner","fields":[
                    {"name":"x","type":"int"},
                    {"name":"amount","type":{"type":"bytes","logicalType":"decimal","precision":5,"scale":1}}]}}
            ]}"#,
        )
        .unwrap();
        let mut writer = Writer::new(&schema, Vec::new()).unwrap();
        let mut r = Record::new(&schema).unwrap();
        r.put("id", 1i64);
        r.put(
            "name",
            Value::Union(1, Box::new(Value::String("Jackie".into()))),
        );
        r.put("ok", true);
        r.put("score", 1.5f64);
        r.put("color", Value::Enum(1, "GREEN".into()));
        r.put("d", Value::Date(19737));
        r.put("t", Value::TimeMicros(3_723_500_000));
        r.put("ts", Value::TimestampMillis(1_700_000_000_123));
        r.put("lts", Value::LocalTimestampMicros(1_700_000_000_000_000));
        r.put(
            "price",
            Value::Union(
                1,
                Box::new(Value::Decimal(apache_avro::Decimal::from(vec![0x30, 0x39]))),
            ),
        );
        r.put(
            "u",
            Value::Uuid(uuid::Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap()),
        );
        r.put("raw", Value::Bytes(vec![0xde, 0xad]));
        r.put("tags", Value::Array(vec![Value::String("a\"b".into())]));
        r.put(
            "attrs",
            Value::Map([("k".to_string(), Value::Long(2))].into_iter().collect()),
        );
        r.put(
            "inner",
            Value::Record(vec![
                ("x".into(), Value::Int(3)),
                (
                    "amount".into(),
                    Value::Decimal(apache_avro::Decimal::from(vec![0xff, 0xfb])),
                ),
            ]),
        );
        writer.append_value(r).unwrap();
        let mut r = Record::new(&schema).unwrap();
        r.put("id", 2i64);
        r.put("name", Value::Union(0, Box::new(Value::Null)));
        r.put("ok", false);
        r.put("score", f64::NAN);
        r.put("color", Value::Enum(0, "RED".into()));
        r.put("d", Value::Date(0));
        r.put("t", Value::TimeMicros(0));
        r.put("ts", Value::TimestampMillis(0));
        r.put("lts", Value::LocalTimestampMicros(0));
        r.put("price", Value::Union(0, Box::new(Value::Null)));
        r.put(
            "u",
            Value::Uuid(uuid::Uuid::parse_str("67e55044-10b1-426f-9247-bb680e5fe0c8").unwrap()),
        );
        r.put("raw", Value::Bytes(vec![]));
        r.put("tags", Value::Array(vec![]));
        r.put("attrs", Value::Map(Default::default()));
        r.put(
            "inner",
            Value::Record(vec![
                ("x".into(), Value::Int(0)),
                (
                    "amount".into(),
                    Value::Decimal(apache_avro::Decimal::from(vec![0x00])),
                ),
            ]),
        );
        writer.append_value(r).unwrap();
        let out = json_lines(writer.into_inner().unwrap());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                r#"{"id":1,"name":"Jackie","ok":true,"score":1.5,"color":"GREEN","d":"2024-01-15","t":"01:02:03.5","ts":"2023-11-14T22:13:20.123Z","lts":"2023-11-14 22:13:20","price":123.45,"u":"67e55044-10b1-426f-9247-bb680e5fe0c8","raw":"dead","tags":["a\"b"],"attrs":{"k":2},"inner":{"x":3,"amount":-0.5}}"#,
                r#"{"id":2,"name":null,"ok":false,"score":null,"color":"RED","d":"1970-01-01","t":"00:00:00","ts":"1970-01-01T00:00:00Z","lts":"1970-01-01 00:00:00","price":null,"u":"67e55044-10b1-426f-9247-bb680e5fe0c8","raw":"","tags":[],"attrs":{},"inner":{"x":0,"amount":0.0}}"#,
            ]
        );
    }

    #[test]
    fn reused_named_types() {
        // `money` and `Item` are defined once and then used by name (`Schema::Ref`)
        let schema = Schema::parse_str(
            r#"{"type":"record","name":"Row","namespace":"shop","fields":[
                {"name":"a","type":{"type":"fixed","name":"money","size":4,"logicalType":"decimal","precision":9,"scale":2}},
                {"name":"b","type":"money"},
                {"name":"c","type":["null","shop.money"]},
                {"name":"first","type":{"type":"record","name":"Item","fields":[
                    {"name":"price","type":"money"}]}},
                {"name":"second","type":"Item"},
                {"name":"items","type":{"type":"array","items":"Item"}}
            ]}"#,
        )
        .unwrap();
        let money = || Value::Decimal(apache_avro::Decimal::from(vec![0x00, 0x00, 0x30, 0x39]));
        let item = || Value::Record(vec![("price".into(), money())]);
        let mut writer = Writer::new(&schema, Vec::new()).unwrap();
        let mut r = Record::new(&schema).unwrap();
        r.put("a", money());
        r.put("b", money());
        r.put("c", Value::Union(1, Box::new(money())));
        r.put("first", item());
        r.put("second", item());
        r.put("items", Value::Array(vec![item()]));
        writer.append_value(r).unwrap();
        let data = writer.into_inner().unwrap();
        assert_eq!(
            json_lines(data.clone()),
            "{\"a\":123.45,\"b\":123.45,\"c\":123.45,\"first\":{\"price\":123.45},\
             \"second\":{\"price\":123.45},\"items\":[{\"price\":123.45}]}\n"
        );

        let reader: Box<dyn Read + Send> = Box::new(io::Cursor::new(data));
        let mut table = AvroJson::reader(reader, "t").table().unwrap();
        let types: Vec<String> = table.columns().into_iter().map(|(_, t)| t).collect();
        assert_eq!(
            types,
            [
                "DECIMAL(9,2)",
                "DECIMAL(9,2)",
                "DECIMAL(9,2)",
                "RECORD",
                "RECORD",
                "ARRAY"
            ]
        );
        let cells = table.next_row().unwrap().unwrap();
        assert!(matches!(&cells[1], Cell::Num(t) if t == "123.45"));
        assert!(matches!(&cells[2], Cell::Num(t) if t == "123.45"));
    }

    #[test]
    fn compressed_and_non_record() {
        let schema = Schema::parse_str(r#""string""#).unwrap();
        let mut writer =
            Writer::with_codec(&schema, Vec::new(), apache_avro::Codec::Snappy).unwrap();
        writer.append_value(Value::String("x".into())).unwrap();
        let out = json_lines(writer.into_inner().unwrap());
        assert_eq!(out, "{\"value\":\"x\"}\n");
    }

    #[test]
    fn detects_files() {
        assert!(is_avro_file("s3://bucket/a.avro"));
        assert!(is_avro_file("http://host/a.AVRO?x=1"));
        assert!(!is_avro_file("a.parquet"));
    }
}
