use std::collections::HashMap;
use std::sync::Mutex;
use lazy_static::lazy_static;
use miniserde::json;
use miniserde::json::{Value};
use serde_json_path::JsonPath;
use crate::runtime::{Int, Str, StrMap, IntMap, Float};
use crate::runtime::str_escape::escape_json;


pub(crate) fn map_int_int_to_json(arr: &IntMap<Int>) -> String {
    let mut items: Vec<Int> = vec![];
    arr.iter(|map| {
        for (_, value) in map {
            items.push(*value);
        }
    });
    json::to_string(&items)
}

pub(crate) fn map_int_float_to_json(arr: &IntMap<Float>) -> String {
    let mut items: Vec<Float> = vec![];
    arr.iter(|map| {
        for (_, value) in map {
            items.push(*value);
        }
    });
    json::to_string(&items)
}

pub(crate) fn map_int_str_to_json(arr: &IntMap<Str>) -> String {
    let mut items: Vec<String> = vec![];
    arr.iter(|map| {
        for (_, value) in map {
            items.push(value.to_string());
        }
    });
    json::to_string(&items)
}

pub(crate) fn map_str_int_to_json(obj: &StrMap<Int>) -> String {
    let mut json_obj: HashMap<String, Int> = HashMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            json_obj.insert(key.to_string(), *value);
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn map_str_float_to_json(obj: &StrMap<Float>) -> String {
    let mut json_obj: HashMap<String, Float> = HashMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            json_obj.insert(key.to_string(), *value);
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn map_str_str_to_json(obj: &StrMap<Str>) -> String {
    let mut json_obj: HashMap<String, String> = HashMap::new();
    obj.iter(|map| {
        for (key, value) in map {
            if !value.is_empty() {
                json_obj.insert(key.to_string(), value.to_string());
            }
        }
    });
    json::to_string(&json_obj)
}

pub(crate) fn str_to_json(text: &str) -> String {
    return format!("\"{}\"", escape_json(text));
}

pub(crate) fn from_json(json_text: &str) -> StrMap<Str> {
    if json_text.starts_with('[') {
        return from_json_array(json_text);
    }
    let mut map = hashbrown::HashMap::new();
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
                _ => {}
            }
        }
    }
    StrMap::from(map)
}

fn from_json_array(json_text: &str) -> StrMap<Str> {
    let mut map = hashbrown::HashMap::new();
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
                    map.insert(Str::from(key), Str::from(json::to_string("")));
                }
            }
        }
    }
    StrMap::from(map)
}

lazy_static! {
    static ref JSON_PATHS: Mutex<HashMap<String, JsonPath>> = Mutex::new(HashMap::new());
}

pub(crate) fn json_value(json_text: &str, json_path: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_text) {
        let mut pool = JSON_PATHS.lock().unwrap();
        let json_path = pool.entry(json_path.to_string()).or_insert_with(|| {
            JsonPath::parse(json_path).unwrap()
        });
        if let Some(node) = json_path.query(&json).first() {
            return node.to_string().trim_matches('"').to_owned();
        }
    }
    "".to_owned()
}

pub(crate) fn json_query<'a>(json_text: &str, json_path: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_text) {
        let mut pool = JSON_PATHS.lock().unwrap();
        let json_path = pool.entry(json_path.to_string()).or_insert_with(|| {
            JsonPath::parse(json_path).unwrap()
        });
        for (i, item) in json_path.query(&json).iter().enumerate() {
            map.insert((i + 1) as i64, Str::from(item.to_string()));
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
