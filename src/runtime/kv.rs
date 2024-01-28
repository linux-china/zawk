use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use lazy_static::lazy_static;
use miniserde::json;
use nats::jetstream::JetStream;
use redis::Commands;

pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
    if is_redis_url(namespace) {
        return redis_kv_get(namespace, key);
    } else if is_nats_url(namespace) {
        return nats_kv_get(namespace, key);
    }
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
    if is_redis_url(namespace) {
        return redis_kv_put(namespace, key, value);
    } else if is_nats_url(namespace) {
        return nats_kv_put(namespace, key, value);
    }
    let json_file = kv_dir().join(format!("{}.json", namespace));
    let mut json_obj = read_json_file(&json_file);
    json_obj.insert(key.to_string(), value.to_string());
    std::fs::write(json_file, json::to_string(&json_obj)).unwrap();
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    if is_redis_url(namespace) {
        return redis_kv_delete(namespace, key);
    } else if is_nats_url(namespace) {
        return nats_kv_delete(namespace, key);
    }
    let json_file = kv_dir().join(format!("{}.json", namespace));
    let mut json_obj = read_json_file(&json_file);
    json_obj.remove(key);
    std::fs::write(json_file, json::to_string(&json_obj)).unwrap();
}

pub(crate) fn kv_clear(namespace: &str) {
    if is_redis_url(namespace) {
        return redis_kv_clear(namespace);
    } else if is_nats_url(namespace) {
        return nats_kv_clear(namespace);
    }
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
    static ref REDIS_CONNECTIONS: Mutex<HashMap<String, redis::Connection>> = Mutex::new(HashMap::new());
    static ref NATS_JETSTREAM: Mutex<HashMap<String, nats::jetstream::JetStream>> = Mutex::new(HashMap::new());
}

fn is_redis_url(namespace: &str) -> bool {
    namespace.starts_with("redis://") || namespace.starts_with("redis+tls://")
}

fn is_nats_url(namespace: &str) -> bool {
    namespace.starts_with("nats://")
}

struct RedisHashOperation<'a> {
    pub conn_url: &'a str,
    pub hash_key: &'a str,
}

struct NatsKvOperation<'a> {
    pub conn_url: &'a str,
    pub bucket: &'a str,
}

impl<'a> RedisHashOperation<'a> {
    pub fn from(url_text: &'a str) -> Self {
        let offset = url_text.rfind('/').unwrap();
        let hash_key = &url_text[offset + 1..];
        let conn_url = &url_text[0..(url_text.len() - hash_key.len() - 1)];
        Self {
            conn_url,
            hash_key,
        }
    }
}

impl<'a> NatsKvOperation<'a> {
    pub fn from(url_text: &'a str) -> Self {
        let offset = url_text.rfind('/').unwrap();
        let bucket = &url_text[offset + 1..];
        let conn_url = &url_text[0..(url_text.len() - bucket.len() - 1)];
        Self {
            conn_url,
            bucket,
        }
    }
}

pub(crate) fn redis_kv_get(url_text: &str, key: &str) -> String {
    let operation = RedisHashOperation::from(url_text);
    let mut pool = REDIS_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let client = redis::Client::open(operation.conn_url).unwrap();
        client.get_connection().unwrap()
    });
    conn.hget(operation.hash_key, key).unwrap_or("".to_owned())
}

pub(crate) fn redis_kv_put(url_text: &str, key: &str, value: &str) {
    let operation = RedisHashOperation::from(url_text);
    let mut pool = REDIS_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let client = redis::Client::open(operation.conn_url).unwrap();
        client.get_connection().unwrap()
    });
    conn.hset::<&str, &str, &str, i32>(operation.hash_key, key, value).unwrap();
}

pub(crate) fn redis_kv_delete(url_text: &str, key: &str) {
    let operation = RedisHashOperation::from(url_text);
    let mut pool = REDIS_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let client = redis::Client::open(operation.conn_url).unwrap();
        client.get_connection().unwrap()
    });
    conn.hdel::<&str, &str, i32>(operation.hash_key, key).unwrap();
}

pub(crate) fn redis_kv_clear(url_text: &str) {
    let operation = RedisHashOperation::from(url_text);
    let mut pool = REDIS_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let client = redis::Client::open(operation.conn_url).unwrap();
        client.get_connection().unwrap()
    });
    conn.del::<&str, i32>(operation.hash_key).unwrap();
}

pub(crate) fn nats_kv_get(url_text: &str, key: &str) -> String {
    let operation = NatsKvOperation::from(url_text);
    let mut pool = NATS_JETSTREAM.lock().unwrap();
    let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let nc = nats::connect(operation.conn_url).unwrap();
        nats::jetstream::new(nc)
    });
    let store = nats_kv_store(&jetstream, operation.bucket);
    if let Ok(value) = store.get(key) {
        if let Some(bytes) = value {
            return String::from_utf8(bytes).unwrap();
        }
    }
    "".to_owned()
}

pub(crate) fn nats_kv_put(url_text: &str, key: &str, value: &str) {
    let operation = NatsKvOperation::from(url_text);
    let mut pool = NATS_JETSTREAM.lock().unwrap();
    let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let nc = nats::connect(operation.conn_url).unwrap();
        nats::jetstream::new(nc)
    });
    let store = nats_kv_store(&jetstream, operation.bucket);
    store.put(key, value).unwrap();
}

pub(crate) fn nats_kv_delete(url_text: &str, key: &str) {
    let operation = NatsKvOperation::from(url_text);
    let mut pool = NATS_JETSTREAM.lock().unwrap();
    let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let nc = nats::connect(operation.conn_url).unwrap();
        nats::jetstream::new(nc)
    });
    let store = nats_kv_store(&jetstream, operation.bucket);
    store.delete(key).unwrap();
}

pub(crate) fn nats_kv_clear(_url_text: &str) {
    /*let operation = NatsKvOperation::from(url_text);
    let mut pool = NATS_JETSTREAM.lock().unwrap();
    let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
        let nc = nats::connect(operation.conn_url).unwrap();
        nats::jetstream::new(nc)
    });
    jetstream.delete_key_value(operation.bucket).unwrap();*/
}

fn nats_kv_store(jetstream: &JetStream, bucket: &str) -> nats::kv::Store {
    if let Ok(store) = jetstream.key_value(bucket) {
        store
    } else {
        jetstream.create_key_value(&nats::kv::Config {
            bucket: bucket.to_string(),
            ..Default::default()
        }).unwrap()
    }
}


#[cfg(test)]
mod tests {
    use std::any::Any;
    use crate::bytecode::Reg;
    use crate::compile::Ty;
    use crate::runtime::{Int, Str};
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
    fn test_redis_operations() {
        let namespace = "redis://localhost:6379/demo1";
        let key = "nick";
        redis_kv_put(namespace, key, "Jackie");
        let mut value = redis_kv_get(namespace, key);
        assert_eq!(value, "Jackie");
        redis_kv_delete(namespace, key);
        value = redis_kv_get(namespace, key);
        assert_eq!(value, "");
    }

    #[test]
    fn test_parse_redis_url() {
        let url = "redis://localhost:6379/demo1";
        let operation = RedisHashOperation::from(url);
        println!("{}", operation.conn_url);
    }

    #[test]
    fn test_parse_nats_url() {
        let url = "nats://localhost:4222/bucket1";
        let operation = NatsKvOperation::from(url);
        println!("{}", operation.conn_url);
        println!("{}", operation.bucket);
    }

    #[test]
    fn test_nats_get() {
        let value = "Jackie";
        let url = "nats://localhost:4222/bucket2";
        nats_kv_put(url, "nick", value);
        let value = nats_kv_get(url, "nick");
        println!("{}", value);
    }
}