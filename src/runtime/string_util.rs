use std::collections::HashMap;
use miniserde::json;
use pad::{Alignment, PadStr};
use crate::runtime::{SharedMap, Str, StrMap};

pub fn pad_left(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Left, false)
}

pub fn pad_right(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Right, false)
}

pub fn pad_both(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::MiddleRight, false)
}

pub fn strcmp(text1: &str, text2: &str) -> i64 {
    return if text1 == text2 {
        0
    } else if text1 < text2 {
        -1
    } else {
        1
    };
}

pub fn read_all(path: &str) -> String {
    std::fs::read_to_string(path).unwrap()
}

pub fn write_all(path: &str, content: &str) {
    std::fs::write(path, content).unwrap()
}

pub(crate) fn pairs<'a>(text: &str, pair_sep: &str, kv_sep: &str) -> StrMap<'a, Str<'a>> {
    let is_url_query = pair_sep == "&" && kv_sep == "=";
    let mut map = hashbrown::HashMap::new();
    text.trim_matches(|c| c == '"' || c == '\'').split(pair_sep).for_each(|pair| {
        let kv: Vec<&str> = pair.split(kv_sep).collect();
        if kv.len() == 2 && !kv[1].is_empty() {
            let mut value = kv[1].to_string();
            if is_url_query {
                if let Ok(param_value) = urlencoding::decode(kv[1]) {
                    value = param_value.to_string();
                }
            }
            map.insert(Str::from(kv[0].to_string()), Str::from(value));
        }
    });
    SharedMap::from(map)
}

pub(crate) fn attributes(text: &str) -> StrMap<Str> {
    let mut map = hashbrown::HashMap::new();
    if text.contains('{') {
        let offset = text.find('{').unwrap();
        let name = text[0..offset].trim().to_string();
        map.insert(Str::from("_".to_owned()), Str::from(name));
        let yaml_text = text[offset..].to_string();
        let yaml_object = serde_yaml::from_str::<HashMap<String, serde_yaml::Value>>(&yaml_text).unwrap();
        for (key, value) in yaml_object {
            if let Some(s) = yaml_value_to_string(&value) {
                map.insert(Str::from(key), Str::from(s));
            }
        }
    } else {
        map.insert(Str::from("_".to_owned()), Str::from(text));
    }
    SharedMap::from(map)
}

fn yaml_value_to_string(value: &serde_yaml::Value) -> Option<String> {
    use serde_yaml::Value;
    match value {
        Value::Bool(b) => {
            if *b {
                Some("1".to_owned())
            } else {
                Some("0".to_owned())
            }
        }
        Value::Number(num) => {
            Some(num.to_string())
        }
        Value::String(s) => {
            Some(s.clone())
        }
        Value::Sequence(arr) => {
            let mut items: Vec<String> = vec![];
            for item in arr.iter() {
                if let Some(s) = yaml_value_to_string(item) {
                    items.push(s);
                }
            }
            Some(json::to_string(&items))
        }
        Value::Mapping(_obj) => {
            Some("{}".to_owned())
        }
        _ => { None }
    }
}

#[cfg(test)]
mod tests {
    use unicode_segmentation::UnicodeSegmentation;
    use super::*;

    #[test]
    fn test_pad_left() {
        let text = pad_left("hello", 100, "*");
        println!("{}", text);
    }

    #[test]
    fn test_strcmp() {
        let text1 = "hello";
        let text2 = "Hello";
        println!("{}", strcmp(text1, text2));
    }

    #[test]
    fn test_words() {
        let text = "Hello , world! could you give a 名称?";
        let words = text.unicode_words();
        for word in words {
            println!("{}", word);
        }
    }

    #[test]
    fn test_repeat() {
        let text = "12";
        let result = text.repeat(3);
        println!("{}", result);
    }

    #[test]
    fn test_read_all() {
        let content = read_all("demo.awk");
        println!("{}", content);
    }

    #[test]
    fn test_write_all() {
        let content = "hello";
        write_all("demo2.txt", content);
        write_all("demo2.txt", "hello2");
    }

    #[test]
    fn test_pairs() {
        let text = "name=hello;age=12";
        let map = pairs(text, ";", "=");
        println!("{:?}", map);
    }

    #[test]
    fn test_attributes() {
        let text = r#"http_requests_total{method="post",code="200"}"#;
        let map = attributes(text);
        println!("{:?}", map);
    }
}
