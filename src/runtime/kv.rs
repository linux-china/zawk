use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;

pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
    if is_redis_url(namespace) {
        redis_kv::kv_get(namespace, key)
    } else if is_nats_url(namespace) {
        nats_kv::kv_get(namespace, key)
    } else {
        sqlite_kv::kv_get(namespace, key)
    }
}

pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
    if is_redis_url(namespace) {
        redis_kv::kv_put(namespace, key, value)
    } else if is_nats_url(namespace) {
        nats_kv::kv_put(namespace, key, value)
    } else {
        sqlite_kv::kv_put(namespace, key, value)
    }
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    if is_redis_url(namespace) {
        redis_kv::kv_delete(namespace, key)
    } else if is_nats_url(namespace) {
        nats_kv::kv_delete(namespace, key)
    } else {
        sqlite_kv::kv_delete(namespace, key)
    }
}

pub(crate) fn kv_clear(namespace: &str) {
    if is_redis_url(namespace) {
        redis_kv::kv_clear(namespace)
    } else if is_nats_url(namespace) {
        nats_kv::kv_clear(namespace)
    } else {
        sqlite_kv::kv_clear(namespace)
    }
}

lazy_static! {
    static ref SQLITE_CONNECTIONS: Arc<Mutex<HashMap<String, rusqlite::Connection>>> = Arc::new(Mutex::new(HashMap::new()));
    static ref REDIS_CONNECTIONS: Arc<Mutex<HashMap<String, redis::Connection>>> = Arc::new(Mutex::new(HashMap::new()));
    static ref NATS_JETSTREAM: Arc<Mutex<HashMap<String, nats::jetstream::JetStream>>> = Arc::new(Mutex::new(HashMap::new()));
}

fn is_redis_url(namespace: &str) -> bool {
    namespace.starts_with("redis://") || namespace.starts_with("redis+tls://")
}

fn is_nats_url(namespace: &str) -> bool {
    namespace.starts_with("nats://")
}

mod redis_kv {
    use crate::runtime::kv::REDIS_CONNECTIONS;
    use redis::Commands;

    pub struct RedisHashOperation<'a> {
        pub conn_url: &'a str,
        pub hash_key: &'a str,
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

    pub(crate) fn kv_get(url_text: &str, key: &str) -> String {
        let operation = RedisHashOperation::from(url_text);
        let mut pool = REDIS_CONNECTIONS.lock().unwrap();
        let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let client = redis::Client::open(operation.conn_url).unwrap();
            client.get_connection().unwrap()
        });
        conn.hget(operation.hash_key, key).unwrap_or("".to_owned())
    }

    pub(crate) fn kv_put(url_text: &str, key: &str, value: &str) {
        let operation = RedisHashOperation::from(url_text);
        let mut pool = REDIS_CONNECTIONS.lock().unwrap();
        let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let client = redis::Client::open(operation.conn_url).unwrap();
            client.get_connection().unwrap()
        });
        conn.hset::<&str, &str, &str, i32>(operation.hash_key, key, value).unwrap();
    }

    pub(crate) fn kv_delete(url_text: &str, key: &str) {
        let operation = RedisHashOperation::from(url_text);
        let mut pool = REDIS_CONNECTIONS.lock().unwrap();
        let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let client = redis::Client::open(operation.conn_url).unwrap();
            client.get_connection().unwrap()
        });
        conn.hdel::<&str, &str, i32>(operation.hash_key, key).unwrap();
    }

    pub(crate) fn kv_clear(url_text: &str) {
        let operation = RedisHashOperation::from(url_text);
        let mut pool = REDIS_CONNECTIONS.lock().unwrap();
        let conn = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let client = redis::Client::open(operation.conn_url).unwrap();
            client.get_connection().unwrap()
        });
        conn.del::<&str, i32>(operation.hash_key).unwrap();
    }
}

mod nats_kv {
    use nats::jetstream::JetStream;
    use crate::runtime::kv::NATS_JETSTREAM;

    pub struct NatsKvOperation<'a> {
        pub conn_url: &'a str,
        pub bucket: &'a str,
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

    pub(crate) fn kv_get(url_text: &str, key: &str) -> String {
        let operation = NatsKvOperation::from(url_text);
        let mut pool = NATS_JETSTREAM.lock().unwrap();
        let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let nc = nats::connect(operation.conn_url).unwrap();
            nats::jetstream::new(nc)
        });
        let store = kv_store(&jetstream, operation.bucket);
        if let Ok(value) = store.get(key) {
            if let Some(bytes) = value {
                return String::from_utf8(bytes).unwrap();
            }
        }
        "".to_owned()
    }

    pub(crate) fn kv_put(url_text: &str, key: &str, value: &str) {
        let operation = NatsKvOperation::from(url_text);
        let mut pool = NATS_JETSTREAM.lock().unwrap();
        let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let nc = nats::connect(operation.conn_url).unwrap();
            nats::jetstream::new(nc)
        });
        let store = kv_store(&jetstream, operation.bucket);
        store.put(key, value).unwrap();
    }

    pub(crate) fn kv_delete(url_text: &str, key: &str) {
        let operation = NatsKvOperation::from(url_text);
        let mut pool = NATS_JETSTREAM.lock().unwrap();
        let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let nc = nats::connect(operation.conn_url).unwrap();
            nats::jetstream::new(nc)
        });
        let store = kv_store(&jetstream, operation.bucket);
        store.delete(key).unwrap();
    }

    pub(crate) fn kv_clear(_url_text: &str) {
        /*let operation = NatsKvOperation::from(url_text);
        let mut pool = NATS_JETSTREAM.lock().unwrap();
        let jetstream = pool.entry(operation.conn_url.to_string()).or_insert_with(|| {
            let nc = nats::connect(operation.conn_url).unwrap();
            nats::jetstream::new(nc)
        });
        jetstream.delete_key_value(operation.bucket).unwrap();*/
    }

    fn kv_store(jetstream: &JetStream, bucket: &str) -> nats::kv::Store {
        if let Ok(store) = jetstream.key_value(bucket) {
            store
        } else {
            jetstream.create_key_value(&nats::kv::Config {
                bucket: bucket.to_string(),
                ..Default::default()
            }).unwrap()
        }
    }
}

mod sqlite_kv {
    use rusqlite::{Connection, OptionalExtension};
    use crate::runtime::kv::{SQLITE_CONNECTIONS};

    pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
        let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
        let conn = pool.entry("local".to_owned()).or_insert_with(|| {
            create_sqlite_kv_conn()
        });
        let real_key = format!("{}.{}", namespace, key);
        let mut stmt = conn.prepare_cached("SELECT value FROM kv WHERE key = ?").unwrap();
        let value = stmt.query_row(rusqlite::params![real_key], |row| row.get(0))
            .optional().unwrap();
        value.unwrap_or("".to_owned())
    }

    pub(crate) fn kv_delete(namespace: &str, key: &str) {
        let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
        let conn = pool.entry("local".to_owned()).or_insert_with(|| {
            create_sqlite_kv_conn()
        });
        let real_key = format!("{}.{}", namespace, key);
        let mut stmt = conn.prepare_cached("DELETE FROM kv WHERE key = ?").unwrap();
        stmt.execute(rusqlite::params![real_key]).unwrap();
    }

    pub(crate) fn kv_clear(namespace: &str) {
        let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
        let conn = pool.entry("local".to_owned()).or_insert_with(|| {
            create_sqlite_kv_conn()
        });
        let key_name_pattern = format!("{}.%", namespace);
        let mut stmt = conn.prepare_cached("DELETE FROM kv WHERE key like ?").unwrap();
        stmt.execute(rusqlite::params![key_name_pattern]).unwrap();
    }

    pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
        let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
        let conn = pool.entry("local".to_owned()).or_insert_with(|| {
            create_sqlite_kv_conn()
        });
        let real_key = format!("{}.{}", namespace, key);
        let mut stmt = conn.prepare_cached("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)").unwrap();
        stmt.execute(rusqlite::params![real_key, value]).unwrap();
    }

    fn create_sqlite_kv_conn() -> Connection {
        let awk_config_dir = dirs::home_dir().unwrap().join(".awk");
        if !awk_config_dir.exists() {
            std::fs::create_dir_all(&awk_config_dir).unwrap();
        }
        let sqlite_kv_db = awk_config_dir.join("sqlite.db");
        let conn = Connection::open(sqlite_kv_db.as_path()).unwrap();
        conn.set_prepared_statement_cache_capacity(128);
        {
            let mut stmt = conn.prepare_cached(
                "CREATE TABLE IF NOT EXISTS kv (key VARCHAR UNIQUE, value VARCHAR)",
            ).unwrap();
            stmt.execute(rusqlite::params![]).unwrap();
        }
        conn
    }
}


#[cfg(test)]
mod tests {
    use dashmap::DashMap;
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
        redis_kv::kv_put(namespace, key, "Jackie");
        let mut value = redis_kv::kv_get(namespace, key);
        assert_eq!(value, "Jackie");
        redis_kv::kv_delete(namespace, key);
        value = redis_kv::kv_get(namespace, key);
        assert_eq!(value, "");
    }

    #[test]
    fn test_parse_redis_url() {
        let url = "redis://localhost:6379/demo1";
        let operation = redis_kv::RedisHashOperation::from(url);
        println!("{}", operation.conn_url);
    }

    #[test]
    fn test_parse_nats_url() {
        let url = "nats://localhost:4222/bucket1";
        let operation = nats_kv::NatsKvOperation::from(url);
        println!("{}", operation.conn_url);
        println!("{}", operation.bucket);
    }

    #[test]
    fn test_nats_get() {
        let value = "Jackie";
        let url = "nats://localhost:4222/bucket2";
        nats_kv::kv_put(url, "nick", value);
        let value = nats_kv::kv_get(url, "nick");
        println!("{}", value);
    }

    #[test]
    fn test_sqlite_get() {
        let namespace = "demo";
        sqlite_kv::kv_put(namespace, "nick", "Jackie");
        let value = sqlite_kv::kv_get(namespace, "nick");
        println!("{}", value);
        sqlite_kv::kv_clear(namespace);
    }
}
