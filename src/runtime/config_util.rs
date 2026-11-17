use crate::runtime::json::flatten_json_value;
use crate::runtime::{Str, StrMap, stdlib_warning};
use anyhow::Result;
use ini::configparser::ini::Ini;
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;

pub(crate) fn read_config<'b>(file: &str) -> StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    let ext = std::path::Path::new(file)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if ext == "ini" {
        match read_ini(file) {
            Ok(data_map) => {
                for (key, value) in data_map {
                    map.insert(Str::from(key), Str::from(value));
                }
            }
            Err(e) => stdlib_warning("read_config", format!("{}: {}", file, e)),
        }
    } else if ext == "properties" {
        match read_properties(file) {
            Ok(data_map) => {
                for (key, value) in data_map {
                    map.insert(Str::from(key), Str::from(value));
                }
            }
            Err(e) => stdlib_warning("read_config", format!("{}: {}", file, e)),
        }
    } else if ext == "json" {
        return match read_json(file) {
            Ok(map) => map,
            Err(e) => {
                stdlib_warning("read_config", format!("{}: {}", file, e));
                StrMap::default()
            }
        };
    } else {
        stdlib_warning(
            "read_config",
            format!(
                "{}: unsupported config format, expected .ini, .properties or .json",
                file
            ),
        );
    }
    StrMap::from(map)
}

/// Read a JSON config file whose top level must be an object, flattened like `json_flatten`:
/// nested object keys joined with `.` and array items as `key[1]` (1-based); `.`, `[`, `]` and
/// `\` inside a key are escaped with `\`.
fn read_json<'b>(json_file: &str) -> Result<StrMap<'b, Str<'b>>> {
    let text = std::fs::read_to_string(json_file)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let map = StrMap::default();
    match &value {
        serde_json::Value::Object(obj) => {
            if !obj.is_empty() {
                flatten_json_value(&value, &mut String::new(), &map);
            }
            Ok(map)
        }
        other => {
            let kind = match other {
                serde_json::Value::Array(_) => "array",
                serde_json::Value::String(_) => "string",
                serde_json::Value::Number(_) => "number",
                serde_json::Value::Bool(_) => "boolean",
                _ => "null",
            };
            anyhow::bail!("top-level JSON value must be an object, got {}", kind)
        }
    }
}

/// Read an INI file as `section.key` entries. Section and key names keep their case, like the
/// `.properties` and `.json` readers. The default section (keys before any header, or a
/// `[DEFAULT]` header in any case) is always named `default` and its keys are also available
/// without a prefix.
fn read_ini(ini_file: &str) -> Result<HashMap<String, String>> {
    let mut map: HashMap<String, String> = HashMap::new();
    // `Ini::new()` lowercases every section and key name
    let mut config = Ini::new_cs();
    let root_map = config.load(ini_file).map_err(anyhow::Error::msg)?;
    root_map.iter().for_each(|(root_key, child_map)| {
        let is_default = root_key.eq_ignore_ascii_case("default");
        let section = if is_default { "default" } else { root_key.as_str() };
        child_map.iter().for_each(|(key, v)| {
            if let Some(value) = v {
                map.insert(format!("{}.{}", section, key), value.clone());
                if is_default {
                    map.insert(key.clone(), value.clone());
                }
            }
        });
    });
    Ok(map)
}

fn read_properties(properties_file: &str) -> Result<HashMap<String, String>> {
    let f2 = File::open(properties_file)?;
    Ok(java_properties::read(BufReader::new(f2))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_ini() {
        let map = read_ini("tests/demo.ini").unwrap();
        // `[DEFAULT]` is the default section: `default.` prefix and bare keys
        assert_eq!(map.get("default.key1").map(String::as_str), Some("value1"));
        assert_eq!(map.get("key1").map(String::as_str), Some("value1"));
        assert_eq!(map.get("topsecrets.nuclear_launch_codes").map(String::as_str), Some("topsecret"));
        // key case is kept
        assert_eq!(map.get("github.com.User").map(String::as_str), Some("QEDK"));
        assert!(!map.contains_key("github.com.user"));
    }

    #[test]
    fn test_read_ini_keeps_case() {
        let dir = std::env::temp_dir().join(format!("zawk_read_ini_case_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("case.ini");
        std::fs::write(&file, "Top=1\n[Database]\nHost=localhost\nhost=other\n[default]\nLow=2\n").unwrap();
        let map = read_ini(file.to_str().unwrap()).unwrap();
        let get = |k: &str| map.get(k).map(String::as_str);
        assert_eq!(get("Database.Host"), Some("localhost"));
        assert_eq!(get("Database.host"), Some("other"));
        assert_eq!(get("database.host"), None);
        // keys before any header and `[default]` share the default section
        assert_eq!(get("Top"), Some("1"));
        assert_eq!(get("default.Top"), Some("1"));
        assert_eq!(get("Low"), Some("2"));
        assert_eq!(get("default.Low"), Some("2"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_json() {
        let dir = std::env::temp_dir().join(format!("zawk_read_config_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ok = dir.join("ok.json");
        std::fs::write(
            &ok,
            r#"{"name":"zawk","db":{"host":"localhost","port":3306,"ssl":true,"opt":null},"tags":["a","b"],"empty":{}}"#,
        )
        .unwrap();
        let map = read_json(ok.to_str().unwrap()).unwrap();
        let get = |k: &str| map.get(&Str::from(k.to_string())).as_str().to_string();
        assert_eq!(get("name"), "zawk");
        assert_eq!(get("db.host"), "localhost");
        assert_eq!(get("db.port"), "3306");
        assert_eq!(get("db.ssl"), "true");
        assert_eq!(get("db.opt"), "");
        assert_eq!(get("tags[1]"), "a");
        assert_eq!(get("tags[2]"), "b");
        assert_eq!(get("empty"), "{}");
        assert_eq!(map.len(), 8);

        for (i, text) in ["[1,2]", "\"s\"", "42", "true", "null"].iter().enumerate() {
            let p = dir.join(format!("bad{}.json", i));
            std::fs::write(&p, text).unwrap();
            assert!(read_json(p.to_str().unwrap()).is_err(), "{}", text);
        }
        let empty = dir.join("empty.json");
        std::fs::write(&empty, "{}").unwrap();
        assert_eq!(read_json(empty.to_str().unwrap()).unwrap().len(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_read_properties() {
        let map = read_properties("tests/demo.properties").unwrap();
        println!("{:?}", map);
    }
}
