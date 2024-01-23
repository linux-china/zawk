use std::collections::HashMap;
use std::ops::Index;
use std::path::PathBuf;
use std::sync::Mutex;
use lazy_static::lazy_static;
use miniserde::json;
use redis::Commands;
use url::Url;

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

lazy_static! {
    static ref NATS_CONNECTIONS: Mutex<HashMap<String, redis::Connection>> = Mutex::new(HashMap::new());
}

pub(crate) fn redis_kv_get(url_text: &str, key: &str) -> String {
    let offset = url_text.rfind('/').unwrap();
    let hash_key = url_text[offset + 1..].to_string();
    let conn_url = url_text[0..(url_text.len() - hash_key.len() - 1)].to_string();
    let mut pool = NATS_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(conn_url.to_string()).or_insert_with(|| {
        let client = redis::Client::open(conn_url).unwrap();
        client.get_connection().unwrap()
    });
    conn.hget(hash_key, key).unwrap_or("".to_string())
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

    #[test]
    fn test_redis_get() {
        let value = redis_kv_get("redis://localhost:6379/demo1", "nick");
        println!("{}", value);
    }
}