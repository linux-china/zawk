// nats (sync client) is deprecated in favor of async-nats, but still required here
#![allow(deprecated)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use crate::runtime::stdlib_warning;

/// Errors of the kv functions are reported as warnings: kv_get then returns an empty string.
type KvResult<T> = std::result::Result<T, String>;

fn report<T: Default>(func: &str, res: KvResult<T>) -> T {
    res.unwrap_or_else(|msg| {
        stdlib_warning(func, msg);
        T::default()
    })
}

pub(crate) fn kv_get(namespace: &str, key: &str) -> String {
    report("kv_get", if is_redis_url(namespace) {
        redis_kv::kv_get(namespace, key)
    } else if is_nats_url(namespace) {
        nats_kv::kv_get(namespace, key)
    } else {
        sqlite_kv::kv_get(namespace, key)
    })
}

pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
    report("kv_put", if is_redis_url(namespace) {
        redis_kv::kv_put(namespace, key, value)
    } else if is_nats_url(namespace) {
        nats_kv::kv_put(namespace, key, value)
    } else {
        sqlite_kv::kv_put(namespace, key, value)
    })
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    report("kv_delete", if is_redis_url(namespace) {
        redis_kv::kv_delete(namespace, key)
    } else if is_nats_url(namespace) {
        nats_kv::kv_delete(namespace, key)
    } else {
        sqlite_kv::kv_delete(namespace, key)
    })
}

pub(crate) fn kv_clear(namespace: &str) {
    report("kv_clear", if is_redis_url(namespace) {
        redis_kv::kv_clear(namespace)
    } else if is_nats_url(namespace) {
        nats_kv::kv_clear(namespace)
    } else {
        sqlite_kv::kv_clear(namespace)
    })
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

/// Split `scheme://host/name` into the connection URL and the last path segment (the hash key
/// or bucket name).
fn split_name(url_text: &str) -> KvResult<(&str, &str)> {
    let rest = url_text.split_once("://").map_or(url_text, |(_, rest)| rest);
    match rest.rsplit_once('/') {
        Some((host, name)) if !host.is_empty() && !name.is_empty() => {
            Ok((&url_text[..url_text.len() - name.len() - 1], name))
        }
        _ => Err(format!("invalid namespace {:?}, expected scheme://host/name", url_text)),
    }
}

mod redis_kv {
    use super::{split_name, KvResult, REDIS_CONNECTIONS};
    use redis::Commands;

    /// Run `f` with a (pooled) connection, and the name of the hash in the namespace URL.
    fn with_conn<T>(
        url_text: &str,
        f: impl FnOnce(&mut redis::Connection, &str) -> redis::RedisResult<T>,
    ) -> KvResult<T> {
        let (conn_url, hash_key) = split_name(url_text)?;
        let mut pool = REDIS_CONNECTIONS.lock().map_err(|e| e.to_string())?;
        if !pool.contains_key(conn_url) {
            let conn = redis::Client::open(conn_url)
                .and_then(|client| client.get_connection())
                .map_err(|e| format!("failed to connect to {}: {}", conn_url, e))?;
            pool.insert(conn_url.to_string(), conn);
        }
        let conn = pool.get_mut(conn_url).unwrap();
        f(conn, hash_key).map_err(|e| e.to_string())
    }

    pub(crate) fn kv_get(url_text: &str, key: &str) -> KvResult<String> {
        with_conn(url_text, |conn, hash_key| {
            conn.hget::<_, _, Option<String>>(hash_key, key).map(Option::unwrap_or_default)
        })
    }

    pub(crate) fn kv_put(url_text: &str, key: &str, value: &str) -> KvResult<()> {
        with_conn(url_text, |conn, hash_key| conn.hset::<_, _, _, i32>(hash_key, key, value).map(drop))
    }

    pub(crate) fn kv_delete(url_text: &str, key: &str) -> KvResult<()> {
        with_conn(url_text, |conn, hash_key| conn.hdel::<_, _, i32>(hash_key, key).map(drop))
    }

    pub(crate) fn kv_clear(url_text: &str) -> KvResult<()> {
        with_conn(url_text, |conn, hash_key| conn.del::<_, i32>(hash_key).map(drop))
    }
}

mod nats_kv {
    use nats::jetstream::JetStream;
    use super::{split_name, KvResult, NATS_JETSTREAM};

    /// Run `f` with the key-value store named in the namespace URL, created if needed.
    fn with_store<T>(url_text: &str, f: impl FnOnce(&nats::kv::Store) -> std::io::Result<T>) -> KvResult<T> {
        let (conn_url, bucket) = split_name(url_text)?;
        let mut pool = NATS_JETSTREAM.lock().map_err(|e| e.to_string())?;
        if !pool.contains_key(conn_url) {
            let nc = nats::connect(conn_url).map_err(|e| format!("failed to connect to {}: {}", conn_url, e))?;
            pool.insert(conn_url.to_string(), nats::jetstream::new(nc));
        }
        let store = kv_store(&pool[conn_url], bucket)?;
        f(&store).map_err(|e| e.to_string())
    }

    pub(crate) fn kv_get(url_text: &str, key: &str) -> KvResult<String> {
        with_store(url_text, |store| {
            Ok(store.get(key)?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default())
        })
    }

    pub(crate) fn kv_put(url_text: &str, key: &str, value: &str) -> KvResult<()> {
        with_store(url_text, |store| store.put(key, value).map(drop))
    }

    pub(crate) fn kv_delete(url_text: &str, key: &str) -> KvResult<()> {
        with_store(url_text, |store| store.delete(key))
    }

    pub(crate) fn kv_clear(_url_text: &str) -> KvResult<()> {
        // not supported yet: removing all keys of a bucket.
        Ok(())
    }

    fn kv_store(jetstream: &JetStream, bucket: &str) -> KvResult<nats::kv::Store> {
        if let Ok(store) = jetstream.key_value(bucket) {
            return Ok(store);
        }
        jetstream.create_key_value(&nats::kv::Config {
            bucket: bucket.to_string(),
            ..Default::default()
        }).map_err(|e| format!("failed to create bucket {:?}: {}", bucket, e))
    }
}

mod sqlite_kv {
    use rusqlite::{Connection, OptionalExtension};
    use super::{KvResult, SQLITE_CONNECTIONS};

    /// Run `f` with the local SQLite database (~/.awk/sqlite.db).
    fn with_conn<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> KvResult<T> {
        let mut pool = SQLITE_CONNECTIONS.lock().map_err(|e| e.to_string())?;
        if !pool.contains_key("local") {
            pool.insert("local".to_owned(), create_sqlite_kv_conn()?);
        }
        f(&pool["local"]).map_err(|e| e.to_string())
    }

    pub(crate) fn kv_get(namespace: &str, key: &str) -> KvResult<String> {
        let real_key = format!("{}.{}", namespace, key);
        with_conn(|conn| {
            let mut stmt = conn.prepare_cached("SELECT value FROM kv WHERE key = ?")?;
            let value: Option<String> = stmt.query_row(rusqlite::params![real_key], |row| row.get(0)).optional()?;
            Ok(value.unwrap_or_default())
        })
    }

    pub(crate) fn kv_delete(namespace: &str, key: &str) -> KvResult<()> {
        let real_key = format!("{}.{}", namespace, key);
        with_conn(|conn| {
            conn.prepare_cached("DELETE FROM kv WHERE key = ?")?.execute(rusqlite::params![real_key]).map(drop)
        })
    }

    pub(crate) fn kv_clear(namespace: &str) -> KvResult<()> {
        let key_name_pattern = format!("{}.%", namespace);
        with_conn(|conn| {
            conn.prepare_cached("DELETE FROM kv WHERE key like ?")?.execute(rusqlite::params![key_name_pattern]).map(drop)
        })
    }

    pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) -> KvResult<()> {
        let real_key = format!("{}.{}", namespace, key);
        with_conn(|conn| {
            conn.prepare_cached("INSERT OR REPLACE INTO kv (key, value) VALUES (?, ?)")?
                .execute(rusqlite::params![real_key, value])
                .map(drop)
        })
    }

    fn create_sqlite_kv_conn() -> KvResult<Connection> {
        let awk_config_dir = dirs::home_dir().ok_or("cannot find the home directory")?.join(".awk");
        std::fs::create_dir_all(&awk_config_dir)
            .map_err(|e| format!("cannot create {}: {}", awk_config_dir.display(), e))?;
        let sqlite_kv_db = awk_config_dir.join("sqlite.db");
        let open = || -> rusqlite::Result<Connection> {
            let conn = Connection::open(sqlite_kv_db.as_path())?;
            conn.set_prepared_statement_cache_capacity(128);
            conn.execute("CREATE TABLE IF NOT EXISTS kv (key VARCHAR UNIQUE, value VARCHAR)", [])?;
            Ok(conn)
        };
        open().map_err(|e| format!("cannot open {}: {}", sqlite_kv_db.display(), e))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
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
    #[ignore]
    fn test_redis_operations() {
        let namespace = "redis://localhost:6379/demo1";
        let key = "nick";
        redis_kv::kv_put(namespace, key, "Jackie").unwrap();
        let mut value = redis_kv::kv_get(namespace, key).unwrap();
        assert_eq!(value, "Jackie");
        redis_kv::kv_delete(namespace, key).unwrap();
        value = redis_kv::kv_get(namespace, key).unwrap();
        assert_eq!(value, "");
    }

    #[test]
    fn test_split_name() {
        assert_eq!(split_name("redis://localhost:6379/demo1"), Ok(("redis://localhost:6379", "demo1")));
        assert_eq!(split_name("nats://localhost:4222/bucket1"), Ok(("nats://localhost:4222", "bucket1")));
        assert!(split_name("redis://localhost:6379").is_err());
        assert!(split_name("redis://localhost:6379/").is_err());
    }

    #[test]
    fn test_unreachable_server_is_not_fatal() {
        // port 1 is never a redis or nats server: errors are warnings, not panics.
        assert_eq!(kv_get("redis://127.0.0.1:1/demo", "nick"), "");
        kv_put("redis://127.0.0.1:1/demo", "nick", "Jackie");
        assert_eq!(kv_get("nats://127.0.0.1:1/bucket", "nick"), "");
        assert_eq!(kv_get("redis://no-hash-key", "nick"), "");
    }

    #[test]
    #[ignore]
    fn test_nats_get() {
        let value = "Jackie";
        let url = "nats://localhost:4222/bucket2";
        nats_kv::kv_put(url, "nick", value).unwrap();
        let value = nats_kv::kv_get(url, "nick").unwrap();
        println!("{}", value);
    }

    #[test]
    fn test_sqlite_get() {
        let namespace = "demo";
        sqlite_kv::kv_put(namespace, "nick", "Jackie").unwrap();
        let value = sqlite_kv::kv_get(namespace, "nick").unwrap();
        println!("{}", value);
        sqlite_kv::kv_clear(namespace).unwrap();
    }
}
