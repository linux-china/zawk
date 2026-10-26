use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use miniserde::json;
use miniserde::json::{Value};
use serde_json_path::JsonPath;
use crate::runtime::{stdlib_warning, CostModel, Int, Str, StrLru, StrMap, IntMap, Float};
use crate::runtime::str_escape::escape_json;


/// Encode int-keyed entries: a JSON array when the keys are exactly `1..n` (or empty),
/// otherwise a JSON object with keys in numeric order, so `from_json` keeps the keys.
fn int_entries_to_json<T: miniserde::Serialize>(mut entries: Vec<(Int, T)>) -> String {
    entries.sort_by_key(|(key, _)| *key);
    let is_list = entries.iter().enumerate().all(|(i, (key, _))| *key == i as Int + 1);
    if is_list {
        let items: Vec<T> = entries.into_iter().map(|(_, value)| value).collect();
        return json::to_string(&items);
    }
    let fields: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("\"{}\":{}", key, json::to_string(value)))
        .collect();
    format!("{{{}}}", fields.join(","))
}

pub(crate) fn map_int_int_to_json(arr: &IntMap<Int>) -> String {
    let mut entries: Vec<(Int, Int)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, *value));
        }
    });
    int_entries_to_json(entries)
}

pub(crate) fn map_int_float_to_json(arr: &IntMap<Float>) -> String {
    let mut entries: Vec<(Int, Float)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, *value));
        }
    });
    int_entries_to_json(entries)
}

pub(crate) fn map_int_str_to_json(arr: &IntMap<Str>) -> String {
    let mut entries: Vec<(Int, String)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, value.to_string()));
        }
    });
    int_entries_to_json(entries)
}

pub(crate) fn map_str_int_to_json(obj: &StrMap<Int>) -> String {
    let mut json_obj: BTreeMap<String, Int> = BTreeMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            json_obj.insert(key.to_string(), *value);
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn map_str_float_to_json(obj: &StrMap<Float>) -> String {
    let mut json_obj: BTreeMap<String, Float> = BTreeMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            json_obj.insert(key.to_string(), *value);
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn map_str_str_to_json(obj: &StrMap<Str>) -> String {
    let mut json_obj: BTreeMap<String, String> = BTreeMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            json_obj.insert(key.to_string(), value.to_string());
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn str_to_json(text: &str) -> String {
    format!("\"{}\"", escape_json(text))
}

pub(crate) fn from_json<'b>(json_text: &str) -> StrMap<'b, Str<'b>> {
    let new_json_text = json_text.trim();
    if new_json_text.starts_with('[') {
        return from_json_array(new_json_text);
    }
    let mut map = crate::runtime::HashMap::default();
    if new_json_text.is_empty() {
        return StrMap::from(map);
    }
    let Ok(json_obj) = json::from_str::<HashMap<String, Value>>(new_json_text) else {
        stdlib_warning("from_json", format!("not a JSON object or array: {:?}", json_snippet(new_json_text)));
        return StrMap::from(map);
    };
    for (key, value) in json_obj {
        match value {
            Value::Bool(b) => {
                if b {
                    map.insert(Str::from(key), Str::from("1"));
                } else {
                    map.insert(Str::from(key), Str::from("0"));
                }
            }
            Value::Number(num) => {
                map.insert(Str::from(key), Str::from(num.to_string()));
            }
            Value::String(s) => {
                map.insert(Str::from(key), Str::from(s));
            }
            Value::Array(arr) => {
                map.insert(Str::from(key), Str::from(json::to_string(&arr)));
            }
            Value::Object(obj) => {
                map.insert(Str::from(key), Str::from(json::to_string(&obj)));
            }
            Value::Null => {
                map.insert(Str::from(key), Str::default());
            }
        }
    }
    StrMap::from(map)
}

/// The start of `text` for a warning, so that a long document does not flood stderr.
fn json_snippet(text: &str) -> String {
    const MAX_CHARS: usize = 40;
    match text.char_indices().nth(MAX_CHARS) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_string(),
    }
}

fn from_json_array<'b>(json_text: &str) -> StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    if json_text.is_empty() {
        return StrMap::from(map);
    }
    let Ok(json_array) = json::from_str::<Vec<Value>>(json_text) else {
        stdlib_warning("from_json", format!("invalid JSON array: {:?}", json_snippet(json_text)));
        return StrMap::from(map);
    };
    for (index, json_value) in json_array.iter().enumerate() {
        let key = (index + 1).to_string();
        match json_value {
            Value::Bool(b) => {
                if *b {
                    map.insert(Str::from(key), Str::from("1"));
                } else {
                    map.insert(Str::from(key), Str::from("0"));
                }
            }
            Value::Number(num) => {
                map.insert(Str::from(key), Str::from(num.to_string()));
            }
            Value::String(s) => {
                map.insert(Str::from(key), Str::from(s.to_string()));
            }
            Value::Array(arr) => {
                map.insert(Str::from(key), Str::from(json::to_string(&arr)));
            }
            Value::Object(obj) => {
                map.insert(Str::from(key), Str::from(json::to_string(&obj)));
            }
            Value::Null => {
                map.insert(Str::from(key), Str::default());
            }
        }
    }
    StrMap::from(map)
}

/// Memory budget, in bytes, for the compiled JSON paths. Paths can come from input data, so once
/// the estimated size of the cache reaches this budget the least recently used half is evicted.
const JSON_PATHS_BUDGET: usize = 16 << 20;

/// Estimated memory of a compiled JSON path of the given length in bytes.
const JSON_PATH_COST: CostModel = CostModel {
    base: 512,
    per_byte: 32,
};

static JSON_PATHS: LazyLock<Mutex<StrLru<JsonPath>>> = LazyLock::new(|| Mutex::new(StrLru::new(JSON_PATHS_BUDGET, JSON_PATH_COST)));

fn compiled_json_path<'p>(pool: &'p mut StrLru<JsonPath>, func: &str, json_path: &str) -> Option<&'p JsonPath> {
    pool.get_or_try_insert(json_path, || JsonPath::parse(json_path))
        .map_err(|e| stdlib_warning(func, format!("{:?}: {}", json_path, e)))
        .ok()
}

/// Text of a JSONPath result: strings are decoded, null is empty, other values stay JSON text.
fn json_node_text(node: &serde_json::Value) -> String {
    match node {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(crate) fn json_value(json_text: &str, json_path: &str) -> String {
    if !json_text.is_empty() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_text) {
            let mut pool = JSON_PATHS.lock().unwrap_or_else(|e| e.into_inner());
            let Some(json_path) = compiled_json_path(&mut pool, "json_value", json_path) else {
                return "".to_owned();
            };
            if let Some(node) = json_path.query(&json).first() {
                return json_node_text(node);
            }
        }
    }
    "".to_owned()
}

pub(crate) fn json_query<'a>(json_text: &str, json_path: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if !json_text.is_empty() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_text) {
            let mut pool = JSON_PATHS.lock().unwrap_or_else(|e| e.into_inner());
            let Some(json_path) = compiled_json_path(&mut pool, "json_query", json_path) else {
                return map;
            };
            for (i, item) in json_path.query(&json).iter().enumerate() {
                map.insert((i + 1) as i64, Str::from(json_node_text(item)));
            }
        }
    }
    map
}


/// Top-level JSON object of the text, `None` for empty, invalid or non-object text.
fn json_object(json_text: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_str::<serde_json::Value>(json_text) {
        Ok(serde_json::Value::Object(obj)) => Some(obj),
        _ => None,
    }
}

pub(crate) fn json_has(json_text: &str, key: &str) -> Int {
    json_object(json_text).is_some_and(|obj| obj.contains_key(key)) as Int
}

pub(crate) fn json_get(json_text: &str, key: &str) -> String {
    json_object(json_text)
        .and_then(|obj| obj.get(key).map(json_node_text))
        .unwrap_or_default()
}

/// Value text that is valid JSON (number, true, `"s"`, `[..]`, `{..}`) is stored as is, other text as a JSON string.
pub(crate) fn json_set(json_text: &str, key: &str, value: &str) -> String {
    let mut obj = if json_text.trim().is_empty() {
        serde_json::Map::new()
    } else {
        match json_object(json_text) {
            Some(obj) => obj,
            None => return json_text.to_owned(),
        }
    };
    let value = serde_json::from_str::<serde_json::Value>(value)
        .unwrap_or_else(|_| serde_json::Value::String(value.to_owned()));
    obj.insert(key.to_owned(), value);
    serde_json::Value::Object(obj).to_string()
}

pub(crate) fn json_del(json_text: &str, key: &str) -> String {
    if let Some(mut obj) = json_object(json_text) {
        if obj.shift_remove(key).is_some() {
            return serde_json::Value::Object(obj).to_string();
        }
    }
    json_text.to_owned()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_array_json() {
        //language=json
        let json_text = r#"  {"id": 1, "names" : ["first", "second"] } "#;
        let json_object = json::from_str::<HashMap<String, Value>>(json_text).unwrap();
        let text = json::to_string(&json_object);
        println!("{}", text);
    }

    #[test]
    fn test_array() {
        //language=json
        let json_text = r#"  [1, 2, "third"] "#;
        let json_array = json::from_str::<Vec<Value>>(json_text).unwrap();
        println!("size: {}", json_array.len());
        let text = json::to_string(&json_array);
        println!("{}", text);
    }

    #[test]
    fn test_int_entries_to_json() {
        assert_eq!(int_entries_to_json::<Int>(vec![]), "[]");
        assert_eq!(int_entries_to_json(vec![(2, 20), (1, 10)]), "[10,20]");
        assert_eq!(int_entries_to_json(vec![(1, 1), (3, 3)]), r#"{"1":1,"3":3}"#);
        assert_eq!(int_entries_to_json(vec![(10, 1), (2, 2)]), r#"{"2":2,"10":1}"#);
        assert_eq!(int_entries_to_json(vec![(0, "a".to_string()), (-1, "b".to_string())]), r#"{"-1":"b","0":"a"}"#);
    }

    #[test]
    fn test_json_value() {
        let json_text = r#"{ "foo": { "bar": ["baz", 42] } }"#;
        println!("{}", json_value(json_text, "$.foo.bar[0]"));
    }
    #[test]
    fn test_json_query() {
        let json_text = r#"{ "books": [{ "name": "a" }, { "name": "b" }] }"#;
        println!("{:?}", json_query(json_text, "$.books[:].name"));
    }
}
