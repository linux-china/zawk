//! JSON document (`-i json`) input support.
//!
//! A JSON document is converted to JSON Lines, which is then read like `-i jsonl` input: `$1..$NF`
//! are the values of an object and `FI` maps each key (of the first record) to its column.
//!
//! The records of a document are selected with an optional JSONPath after the file name, as in
//! `demo.json#$.data`:
//!
//! * an array (the whole document, or the node a singular path such as `$.data` or `$.a[0]`
//!   selects) is one record per element;
//! * an object is a single record;
//! * a string, number, boolean or null is a single record of plain text: `$0` and `$1` are the
//!   value (a string unescaped);
//! * any other path (e.g. `$.data[*]`, `$..x`, a slice or a filter) is one record per node it
//!   selects, however many there are.
//!
//! Nested objects and arrays of a record keep their JSON text. The whole document is read into
//! memory.

use std::collections::HashMap;
use std::io::{self, Read};

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use serde_json_path::JsonPath;

/// Splits `demo.json#$.data` into the file and the JSONPath. The path starts at the last `#$`;
/// a local file whose name contains `#$` is read as is.
pub fn split_path(arg: &str) -> (&str, Option<&str>) {
    match arg.rfind("#$") {
        Some(ix) if !std::path::Path::new(arg).exists() => (&arg[..ix], Some(&arg[ix + 1..])),
        _ => (arg, None),
    }
}

/// Whether `file` (with an optional `#path`) names a JSON document by its extension, after a
/// compression extension.
pub fn is_json_file(file: &str) -> bool {
    let name = split_path(file).0.split('?').next().unwrap_or_default().to_ascii_lowercase();
    let mut name = name.as_str();
    for ext in [".gz", ".zst", ".zstd", ".bz2", ".xz", ".lz4"] {
        if let Some(n) = name.strip_suffix(ext) {
            name = n;
            break;
        }
    }
    name.ends_with(".json")
}

/// The records of a JSON document (from `inner`), as JSON Lines.
pub struct JsonRecords<R> {
    inner: Option<R>,
    // The operand, for errors.
    name: String,
    path: Option<String>,
    buf: Vec<u8>,
    pos: usize,
}

impl<R: Read> JsonRecords<R> {
    /// `name` is the operand: the file, then an optional `#path`.
    pub fn new(inner: R, name: &str) -> JsonRecords<R> {
        let (_, path) = split_path(name);
        JsonRecords {
            inner: Some(inner),
            name: name.to_string(),
            path: path.map(String::from),
            buf: Vec::new(),
            pos: 0,
        }
    }

    fn load(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        to_jsonl(data, self.path.as_deref()).map_err(|e| {
            io::Error::other(format!("cannot read JSON file `{}': {} (use `-i jsonl`, `-i csv` or `-F` to read it as another format)", self.name, e))
        })
    }
}

impl<R: Read> Read for JsonRecords<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(mut inner) = self.inner.take() {
            let mut data = Vec::new();
            inner.read_to_end(&mut data)?;
            self.buf = self.load(&data)?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The records of the JSON document `data` selected by `path` (the whole document if `None`), one
/// compact JSON value per line.
///
/// A record keeps the original text of its values (only whitespace between tokens is dropped), so
/// numbers are never rounded or rejected for being out of range, as with `-i jsonl`.
pub fn to_jsonl(data: &[u8], path: Option<&str>) -> Result<Vec<u8>, String> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    if data.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    let data = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    let doc: &RawValue = match serde_json::from_str(data) {
        Ok(doc) => doc,
        // JSON Lines (or other whitespace separated JSON values) in a `.json` file: one record per
        // value, as with `-i jsonl`.
        Err(e) if path.is_none() => return value_stream(data).ok_or_else(|| e.to_string()),
        Err(e) => return Err(e.to_string()),
    };
    let mut records: Vec<&RawValue> = Vec::new();
    match path {
        None => push_records(doc, &mut records)?,
        Some(path) => {
            let singular = is_singular(path);
            let path = JsonPath::parse(path).map_err(|e| format!("invalid JSONPath `{}': {}", path, e))?;
            // The path is evaluated on a `Value` copy of the document, whose nodes map back to
            // their original text by address.
            let mut raws = HashMap::new();
            let tree = build(doc, &mut raws)?;
            raws.insert(&tree as *const Value, doc);
            let nodes: Vec<&RawValue> = path.query(&tree).all().into_iter().map(|node| raws[&(node as *const Value)]).collect();
            // A singular path (`$.data`, `$.a[0]`) selects at most one node, whose elements are the
            // records; any other path (`[*]`, `..`, slices, filters, unions) is one record per node,
            // whatever the number of nodes.
            match nodes.as_slice() {
                [node] if singular => push_records(node, &mut records)?,
                nodes => records.extend(nodes),
            }
            if records.is_empty() {
                crate::runtime::stdlib_warning("json", format!("JSONPath `{}' selects no records", path));
            }
        }
    }
    let mut out = Vec::new();
    for record in records {
        compact(record.get(), &mut out);
        out.push(b'\n');
    }
    Ok(out)
}

/// The records of `data` as a sequence of several JSON values, one compact value per line, or
/// `None` if it is not one.
fn value_stream(data: &str) -> Option<Vec<u8>> {
    let values = serde_json::Deserializer::from_str(data).into_iter::<&RawValue>();
    let values = values.collect::<Result<Vec<_>, _>>().ok().filter(|values| values.len() > 1)?;
    let mut out = Vec::new();
    for value in values {
        compact(value.get(), &mut out);
        out.push(b'\n');
    }
    Some(out)
}

/// Whether the local file `file` starts like a JSON document (`{` or `[`, after a BOM and
/// whitespace). Other files (remote, compressed or unreadable) are assumed to.
pub fn looks_like_json(file: &str) -> bool {
    let name = split_path(file).0;
    let path = std::path::Path::new(name);
    let compressed = [".gz", ".zst", ".zstd", ".bz2", ".xz", ".lz4"]
        .iter()
        .any(|ext| name.to_ascii_lowercase().ends_with(ext));
    if compressed || !path.is_file() {
        return true;
    }
    let mut head = [0u8; 4096];
    let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) {
        Ok(n) => n,
        Err(_) => return true,
    };
    let head = head[..n].strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&head[..n]);
    match head.iter().find(|b| !b.is_ascii_whitespace()) {
        Some(b) => matches!(b, b'{' | b'['),
        None => true,
    }
}

/// Whether `path` is a singular query (RFC 9535 2.3.5.1: only name and index segments), which
/// selects at most one node. `serde_json_path` doesn't expose it, but a comparison in a filter only
/// accepts a singular query.
fn is_singular(path: &str) -> bool {
    match path.strip_prefix('$') {
        Some(rel) => JsonPath::parse(&format!("$[?@{} == 0]", rel)).is_ok(),
        None => false,
    }
}

fn push_records<'a>(value: &'a RawValue, records: &mut Vec<&'a RawValue>) -> Result<(), String> {
    if value.get().starts_with('[') {
        let items: Vec<&RawValue> = serde_json::from_str(value.get()).map_err(|e| e.to_string())?;
        records.extend(items);
    } else {
        records.push(value);
    }
    Ok(())
}

/// The members of a JSON object, in order, with their original text.
struct Members<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MembersVisitor;
        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Members<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members<'de>, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Members(members))
            }
        }
        deserializer.deserialize_map(MembersVisitor)
    }
}

/// `raw` as a `Value` to evaluate a JSONPath on, recording the original text of each array element
/// and object member in `raws`. A number that does not fit an `f64` saturates (only filters see it).
fn build<'a>(raw: &'a RawValue, raws: &mut HashMap<*const Value, &'a RawValue>) -> Result<Value, String> {
    let text = raw.get();
    let value = match text.as_bytes()[0] {
        b'[' => {
            let items: Vec<&RawValue> = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let values = items.iter().map(|item| build(item, raws)).collect::<Result<Vec<_>, _>>()?;
            // The elements stay at these addresses: moving the `Vec` does not move its buffer.
            for (value, item) in values.iter().zip(items) {
                raws.insert(value as *const Value, item);
            }
            Value::Array(values)
        }
        b'{' => {
            let Members(members) = serde_json::from_str(text).map_err(|e| e.to_string())?;
            let mut map = Map::new();
            for (key, member) in &members {
                map.insert(key.clone(), build(member, raws)?);
            }
            // A repeated key keeps the last value, as `serde_json` does.
            for (key, member) in members {
                raws.insert(&map[&key] as *const Value, member);
            }
            Value::Object(map)
        }
        b'-' | b'0'..=b'9' => match serde_json::from_str(text) {
            Ok(value) => value,
            Err(_) => {
                let n: f64 = text.parse().map_err(|_| format!("invalid number `{}'", text))?;
                Value::from(n.clamp(f64::MIN, f64::MAX))
            }
        },
        _ => serde_json::from_str(text).map_err(|e| e.to_string())?,
    };
    Ok(value)
}

/// Appends the JSON text `json` without the whitespace between tokens.
fn compact(json: &str, out: &mut Vec<u8>) {
    let mut in_string = false;
    let mut escaped = false;
    for &b in json.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
        } else if b == b'"' {
            in_string = true;
        } else if b.is_ascii_whitespace() {
            continue;
        }
        out.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(data: &str, path: Option<&str>) -> String {
        String::from_utf8(to_jsonl(data.as_bytes(), path).unwrap()).unwrap()
    }

    #[test]
    fn array_of_objects() {
        let doc = "[\n {\"id\": 1, \"name\": \"Jackie\", \"tags\": {\"a\": [1, 2]}},\n {\"id\": 2}\n]";
        assert_eq!(lines(doc, None), "{\"id\":1,\"name\":\"Jackie\",\"tags\":{\"a\":[1,2]}}\n{\"id\":2}\n");
    }

    #[test]
    fn json_path() {
        let doc = r#"{"success": true, "msg": "a\nb", "data": [{"id": 1}, {"id": 2}], "n": 3}"#;
        assert_eq!(lines(doc, Some("$.data")), "{\"id\":1}\n{\"id\":2}\n");
        assert_eq!(lines(doc, Some("$.data[*]")), "{\"id\":1}\n{\"id\":2}\n");
        assert_eq!(lines(doc, Some("$.data[1]")), "{\"id\":2}\n");
        assert_eq!(lines(doc, Some("$")), format!("{}\n", serde_json::to_string(&serde_json::from_str::<Value>(doc).unwrap()).unwrap()));
        assert_eq!(lines(doc, Some("$.msg")), "\"a\\nb\"\n");
        assert_eq!(lines(doc, Some("$.n")), "3\n");
        assert_eq!(lines(doc, Some("$.missing")), "");
        assert!(to_jsonl(doc.as_bytes(), Some("data")).is_err());
        assert!(to_jsonl(b"[1,", None).is_err());
        assert!(to_jsonl(b"a b c", None).is_err());
        assert!(to_jsonl(b"{\"a\":1}\n{\"a\":2}\n", Some("$.a")).is_err());
        assert_eq!(lines(" \n", None), "");
    }

    #[test]
    fn original_text() {
        let doc = "[{\"id\": 12345678901234567890123, \"p\": 1.10, \"d\": 0.30000000000000001,\n \"q\": 1e400, \"s\": \"a \\\" b\\u0041\"}]";
        let record = "{\"id\":12345678901234567890123,\"p\":1.10,\"d\":0.30000000000000001,\"q\":1e400,\"s\":\"a \\\" b\\u0041\"}\n";
        assert_eq!(lines(doc, None), record);
        assert_eq!(lines(doc, Some("$")), record);
        assert_eq!(lines(doc, Some("$[*]")), record);
        assert_eq!(lines(doc, Some("$[0].id")), "12345678901234567890123\n");
        assert_eq!(lines(doc, Some("$..q")), "1e400\n");
        assert_eq!(lines(doc, Some("$[?@.p > 1]")), record);
        assert_eq!(lines(doc, Some("$[?@.q > 1e300]")), record);
        assert_eq!(lines(doc, Some("$[?@.p > 2]")), "");
        let doc = r#"{"a": 1, "a": 2.50, "b": [ 1.0 , [2.00] ]}"#;
        assert_eq!(lines(doc, Some("$.a")), "2.50\n");
        assert_eq!(lines(doc, Some("$.b[1]")), "2.00\n");
        assert_eq!(lines(doc, Some("$.b[*]")), "1.0\n[2.00]\n");
        assert_eq!(lines("3.14000", None), "3.14000\n");
    }

    #[test]
    fn singular_path() {
        // Only a singular path expands an array; other paths keep one record per node, however many.
        assert_eq!(lines(r#"{"rows":[[1,2]]}"#, Some("$.rows[*]")), "[1,2]\n");
        assert_eq!(lines(r#"{"rows":[[1,2],[3,4]]}"#, Some("$.rows[*]")), "[1,2]\n[3,4]\n");
        assert_eq!(lines(r#"{"rows":[[1,2]]}"#, Some("$.rows[0]")), "1\n2\n");
        assert_eq!(lines(r#"{"rows":[[1,2]]}"#, Some("$['rows'][-1]")), "1\n2\n");
        assert_eq!(lines(r#"{"a":{"b":[1,2]}}"#, Some("$..b")), "[1,2]\n");
        assert_eq!(lines(r#"{"rows":[[1,2]]}"#, Some("$.rows[0:1]")), "[1,2]\n");
        assert_eq!(lines(r#"{"rows":[[1,2]]}"#, Some("$.rows[?@[0] == 1]")), "[1,2]\n");
        for (path, singular) in [("$", true), ("$.a", true), ("$.a[0].b", true), ("$[\"a'b\"]", true), ("$[*]", false), ("$..a", false), ("$['a','b']", false), ("$[0:1]", false), ("$[?@.a]", false)] {
            assert_eq!(is_singular(path), singular, "{}", path);
        }
    }

    #[test]
    fn json_lines() {
        assert_eq!(lines("{\"a\": 1.50}\n{\"a\": [2]}\n", None), "{\"a\":1.50}\n{\"a\":[2]}\n");
        assert_eq!(lines("{\"a\":1} [1, 2]", None), "{\"a\":1}\n[1,2]\n");
        assert!(to_jsonl(b"{\"a\":1}\n{\"a\":", None).is_err());
    }

    #[test]
    fn split() {
        assert_eq!(split_path("tests/demo.json#$.data"), ("tests/demo.json", Some("$.data")));
        assert_eq!(split_path("https://x/a.json#$.a#$.b"), ("https://x/a.json#$.a", Some("$.b")));
        assert_eq!(split_path("tests/demo.json"), ("tests/demo.json", None));
        assert_eq!(split_path("a.json#x"), ("a.json#x", None));
        assert!(is_json_file("tests/demo.json#$.data"));
        assert!(is_json_file("s3://b/K.JSON.gz"));
        assert!(!is_json_file("a.jsonl"));
    }
}
