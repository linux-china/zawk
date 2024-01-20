use std::collections::HashMap;
use serde_json::Value;
use crate::runtime::{Str, StrMap};

pub fn to_json(obj: &StrMap<Str>) -> String {
    let mut json_obj: HashMap<String,Value> = HashMap::new();
    obj.iter(|map | {
        for (key, value)  in map {
            if !value.is_empty() {
                let value_text = value.as_str();
                if value_text.contains('.') { // check float
                    if let Ok(num) = value_text.parse::<f64>() {
                        json_obj.insert(key.to_string(),Value::from(num));
                    } else {
                        json_obj.insert(key.to_string(),Value::from(value_text));
                    }
                } else { // check integer
                    if let Ok(num) = value_text.parse::<i64>() {
                        json_obj.insert(key.to_string(),Value::from(num));
                    } else {
                        json_obj.insert(key.to_string(),Value::from(value_text));
                    }
                }
            }
        }
    });
    serde_json::to_string(&json_obj).unwrap()
}
