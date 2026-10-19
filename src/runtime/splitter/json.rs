//! JSON document (`-i json`) input support.
//!
//! A JSON document is converted to JSON Lines, which is then read like `-i jsonl` input: `$1..$NF`
//! are the values of an object and `FI` maps each key (of the first record) to its column.
//!
//! The records of a document are selected with an optional JSONPath after the file name, as in
//! `demo.json#$.data`:
//!
//! * an array (the whole document, or the single node a path selects) is one record per element;
//! * an object is a single record;
//! * a string, number, boolean or null is a single record of plain text: `$0` and `$1` are the
//!   value (a string unescaped);
//! * a path selecting several nodes (e.g. `$.data[*]`) is one record per node.
//!
//! Nested objects and arrays of a record keep their JSON text. The whole document is read into
//! memory.

use std::io::{self, Read};

use serde_json::Value;
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
            io::Error::other(format!("cannot read JSON file `{}': {}", self.name, e))
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
pub fn to_jsonl(data: &[u8], path: Option<&str>) -> Result<Vec<u8>, String> {
    let data = data.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(data);
    if data.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(Vec::new());
    }
    let doc: Value = serde_json::from_slice(data).map_err(|e| e.to_string())?;
    let mut records: Vec<&Value> = Vec::new();
    match path {
        None => push_records(&doc, &mut records),
        Some(path) => {
            let path = JsonPath::parse(path).map_err(|e| format!("invalid JSONPath `{}': {}", path, e))?;
            let nodes = path.query(&doc).all();
            match nodes.as_slice() {
                [node] => push_records(node, &mut records),
                nodes => records.extend(nodes),
            }
            if records.is_empty() {
                crate::runtime::stdlib_warning("json", format!("JSONPath `{}' selects no records", path));
            }
        }
    }
    let mut out = Vec::new();
    for record in records {
        serde_json::to_writer(&mut out, record).map_err(|e| e.to_string())?;
        out.push(b'\n');
    }
    Ok(out)
}

fn push_records<'a>(value: &'a Value, records: &mut Vec<&'a Value>) {
    match value {
        Value::Array(items) => records.extend(items),
        value => records.push(value),
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
        assert_eq!(lines(" \n", None), "");
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
