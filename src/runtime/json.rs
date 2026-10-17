use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;
use lazy_static::lazy_static;
use miniserde::json;
use miniserde::json::{Value};
use serde_json_path::JsonPath;
use crate::runtime::{stdlib_warning, CostModel, Int, Str, StrLru, StrMap, IntMap, Float};
use crate::runtime::str_escape::escape_json;


pub(crate) fn map_int_int_to_json(arr: &IntMap<Int>) -> String {
    let mut entries: Vec<(Int, Int)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, *value));
        }
    });
    entries.sort_by_key(|(key, _)| *key);
    let items: Vec<Int> = entries.into_iter().map(|(_, value)| value).collect();
    json::to_string(&items)
}

pub(crate) fn map_int_float_to_json(arr: &IntMap<Float>) -> String {
    let mut entries: Vec<(Int, Float)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, *value));
        }
    });
    entries.sort_by_key(|(key, _)| *key);
    let items: Vec<Float> = entries.into_iter().map(|(_, value)| value).collect();
    json::to_string(&items)
}

pub(crate) fn map_int_str_to_json(arr: &IntMap<Str>) -> String {
    let mut entries: Vec<(Int, String)> = vec![];
    arr.iter(|map| {
        for (key, value) in map {
            entries.push((*key, value.to_string()));
        }
    });
    entries.sort_by_key(|(key, _)| *key);
    let items: Vec<String> = entries.into_iter().map(|(_, value)| value).collect();
    json::to_string(&items)
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
    if json_text.starts_with('[') {
        return from_json_array(json_text);
    }
    let mut map = crate::runtime::HashMap::default();
    if !json_text.is_empty() {
        if let Ok(json_obj) = json::from_str::<HashMap<String, Value>>(json_text) {
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
        }
    }
    StrMap::from(map)
}

fn from_json_array<'b>(json_text: &str) -> StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    if !json_text.is_empty() {
        let result = json::from_str::<Vec<Value>>(json_text);
        if let Ok(json_array) = result {
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

lazy_static! {
    static ref JSON_PATHS: Mutex<StrLru<JsonPath>> = Mutex::new(StrLru::new(JSON_PATHS_BUDGET, JSON_PATH_COST));
}

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
