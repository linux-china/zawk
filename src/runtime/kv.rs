use std::collections::HashMap;
use std::path::PathBuf;
use miniserde::json;

pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
    let json_file = kv_dir().join(format!("{}.json", namespace));
    if json_file.exists() {
        let json_obj = read_json_file(&json_file);
        if let Some(value) = json_obj.get(key) {
            return value.to_owned();
        }
    }
    "".to_owned()
}

pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
    let json_file = kv_dir().join(format!("{}.json", namespace));
    let mut json_obj = read_json_file(&json_file);
    json_obj.insert(key.to_string(), value.to_string());
    std::fs::write(json_file, json::to_string(&json_obj)).unwrap();
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    let json_file = kv_dir().join(format!("{}.json", namespace));
    let mut json_obj = read_json_file(&json_file);
    json_obj.remove(key);
    std::fs::write(json_file, json::to_string(&json_obj)).unwrap();
}

pub(crate) fn kv_clear(namespace: &str) {
    let json_file = kv_dir().join(format!("{}.json", namespace));
    if json_file.exists() {
        std::fs::remove_file(json_file).unwrap();
    }
}

fn kv_dir() -> PathBuf {
    let kv_dir = dirs::home_dir().unwrap().join(".awk").join("kv");
    if !kv_dir.exists() {
        std::fs::create_dir_all(&kv_dir).unwrap();
    }
    kv_dir
}

fn read_json_file(json_file: &PathBuf) -> HashMap<String, String> {
    if json_file.exists() {
        let json_str = std::fs::read_to_string(json_file).unwrap();
        if let Ok(json_obj) = json::from_str::<HashMap<String, String>>(&json_str) {
            return json_obj;
        }
    }
    HashMap::new()
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_put() {
        let namespace = "demo";
        kv_put(namespace, "name", "Jackie");
        kv_put(namespace, "phone", "138xxx");
        assert_eq!(kv_get(namespace, "name"), "Jackie");
        kv_delete(namespace, "name");
        assert_eq!(kv_get(namespace, "name"), "");
        kv_clear(namespace);
    }
}