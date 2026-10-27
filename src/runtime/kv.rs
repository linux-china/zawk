use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use crate::runtime::conn_pool::{ConnPool, ConnSlots};
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
    } else if let Some(err) = unknown_scheme(namespace) {
        Err(err)
    } else {
        sqlite_kv::kv_get(namespace, key)
    })
}

pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) {
    report("kv_put", if is_redis_url(namespace) {
        redis_kv::kv_put(namespace, key, value)
    } else if is_nats_url(namespace) {
        nats_kv::kv_put(namespace, key, value)
    } else if let Some(err) = unknown_scheme(namespace) {
        Err(err)
    } else {
        sqlite_kv::kv_put(namespace, key, value)
    })
}

pub(crate) fn kv_delete(namespace: &str, key: &str) {
    report("kv_delete", if is_redis_url(namespace) {
        redis_kv::kv_delete(namespace, key)
    } else if is_nats_url(namespace) {
        nats_kv::kv_delete(namespace, key)
    } else if let Some(err) = unknown_scheme(namespace) {
        Err(err)
    } else {
        sqlite_kv::kv_delete(namespace, key)
    })
}

pub(crate) fn kv_clear(namespace: &str) {
    report("kv_clear", if is_redis_url(namespace) {
        redis_kv::kv_clear(namespace)
    } else if is_nats_url(namespace) {
        nats_kv::kv_clear(namespace)
    } else if let Some(err) = unknown_scheme(namespace) {
        Err(err)
    } else {
        sqlite_kv::kv_clear(namespace)
    })
}

static SQLITE_CONNECTIONS: LazyLock<ConnSlots<rusqlite::Connection>> = LazyLock::new(ConnSlots::new);
static REDIS_CONNECTIONS: LazyLock<ConnPool<redis::Connection>> = LazyLock::new(ConnPool::new);
static NATS_STORES: LazyLock<Mutex<HashMap<String, async_nats::jetstream::kv::Store>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn is_redis_url(namespace: &str) -> bool {
    ["redis://", "rediss://", "redis+tls://"].iter().any(|scheme| namespace.starts_with(scheme))
}

fn is_nats_url(namespace: &str) -> bool {
    namespace.starts_with("nats://") || namespace.starts_with("nats+tls://")
}

/// A `scheme://...` namespace of an unsupported scheme is an error: storing it as a SQLite
/// namespace would silently write the data to the local database.
fn unknown_scheme(namespace: &str) -> Option<String> {
    let (scheme, _) = namespace.split_once("://")?;
    let is_scheme = !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    is_scheme.then(|| format!(
        "unsupported namespace scheme {:?}, expected redis://, rediss://, redis+tls://, nats://, nats+tls:// or a SQLite name",
        scheme,
    ))
}

/// Split `scheme://host/name` into the connection URL and the last path segment (the hash key
/// or bucket name).
fn split_name(url_text: &str) -> KvResult<(&str, &str)> {
    let rest = url_text.split_once("://").map_or(url_text, |(_, rest)| rest);
    match rest.rsplit_once('/') {
        Some((host, name)) if !host.is_empty() && !name.is_empty() => {
            Ok((&url_text[..url_text.len() - name.len() - 1], name))
        }
        _ => Err(format!("invalid namespace {:?}, expected scheme://host/name", crate::runtime::network::redact_url(url_text))),
    }
}

mod redis_kv {
    use super::{split_name, KvResult, REDIS_CONNECTIONS};
    use redis::Commands;

    fn connect(conn_url: &str) -> KvResult<redis::Connection> {
        crate::runtime::tls::install_default_provider();
        // `redis+tls://` is an alias of `rediss://`, the only TLS scheme the redis crate parses
        let conn_url = &conn_url.replacen("redis+tls://", "rediss://", 1);
        redis::Client::open(conn_url.as_str())
            .and_then(|client| client.get_connection())
            .map_err(|e| format!("failed to connect to {}: {}", crate::runtime::network::redact_url(conn_url), e))
    }

    /// Run `f` with a (pooled) connection, and the name of the hash in the namespace URL.
    /// The connection is taken out of the pool while `f` runs, so no lock is held during I/O.
    /// If a reused connection fails with an IO error (server restart, idle timeout), it is
    /// dropped and `f` is retried once with a new connection.
    fn with_conn<T>(
        url_text: &str,
        f: impl Fn(&mut redis::Connection, &str) -> redis::RedisResult<T>,
    ) -> KvResult<T> {
        let (conn_url, hash_key) = split_name(url_text)?;
        let (mut conn, reused) = match REDIS_CONNECTIONS.take(conn_url) {
            Some(conn) => (conn, true),
            None => (connect(conn_url)?, false),
        };
        match f(&mut conn, hash_key) {
            Err(e) if reused && is_broken(&e) => {
                drop(conn);
                let mut conn = connect(conn_url).map_err(|reconnect| format!("{} ({})", e, reconnect))?;
                let result = f(&mut conn, hash_key);
                if !matches!(&result, Err(e) if is_broken(e)) {
                    REDIS_CONNECTIONS.put(conn_url, conn);
                }
                result.map_err(|e| e.to_string())
            }
            Err(e) if is_broken(&e) => Err(e.to_string()),
            result => {
                REDIS_CONNECTIONS.put(conn_url, conn);
                result.map_err(|e| e.to_string())
            }
        }
    }

    fn is_broken(e: &redis::RedisError) -> bool {
        e.is_io_error() || e.is_connection_dropped() || e.is_unrecoverable_error()
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
    use async_nats::jetstream::kv::{Config, Store};
    use futures_util::TryStreamExt;
    use crate::runtime::network::{nats_client, NATS_RUNTIME};
    use super::{split_name, KvResult, NATS_STORES};

    /// The (pooled) key-value store named in the namespace URL, created if needed.
    fn store(url_text: &str) -> KvResult<Store> {
        let mut pool = NATS_STORES.lock().map_err(|e| e.to_string())?;
        if let Some(store) = pool.get(url_text) {
            return Ok(store.clone());
        }
        let (conn_url, bucket) = split_name(url_text)?;
        let conn_url = conn_url.replacen("nats+tls://", "tls://", 1);
        let client = nats_client(&conn_url)?;
        let store = NATS_RUNTIME.block_on(async {
            let jetstream = async_nats::jetstream::new(client);
            match jetstream.get_key_value(bucket).await {
                Ok(store) => Ok(store),
                Err(_) => jetstream.create_key_value(Config { bucket: bucket.to_string(), ..Default::default() }).await,
            }
        }).map_err(|e| format!("failed to create bucket {:?}: {}", bucket, e))?;
        pool.insert(url_text.to_string(), store.clone());
        Ok(store)
    }

    pub(crate) fn kv_get(url_text: &str, key: &str) -> KvResult<String> {
        let store = store(url_text)?;
        let value = NATS_RUNTIME.block_on(store.get(key)).map_err(|e| e.to_string())?;
        Ok(value.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default())
    }

    pub(crate) fn kv_put(url_text: &str, key: &str, value: &str) -> KvResult<()> {
        let store = store(url_text)?;
        NATS_RUNTIME.block_on(store.put(key, value.to_string().into())).map(drop).map_err(|e| e.to_string())
    }

    pub(crate) fn kv_delete(url_text: &str, key: &str) -> KvResult<()> {
        let store = store(url_text)?;
        NATS_RUNTIME.block_on(store.delete(key)).map_err(|e| e.to_string())
    }

    /// Purge every key of the bucket, history included.
    pub(crate) fn kv_clear(url_text: &str) -> KvResult<()> {
        let store = store(url_text)?;
        NATS_RUNTIME.block_on(async {
            let keys: Vec<String> = store.keys().await.map_err(|e| e.to_string())?.try_collect().await.map_err(|e| e.to_string())?;
            for key in keys {
                store.purge(key).await.map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }
}

mod sqlite_kv {
    use rusqlite::{Connection, OptionalExtension};
    use super::{KvResult, SQLITE_CONNECTIONS};

    /// Run `f` with the local SQLite database (~/.awk/sqlite.db).
    fn with_conn<T>(f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> KvResult<T> {
        SQLITE_CONNECTIONS.with_conn("local", create_sqlite_kv_conn, |conn| f(conn).map_err(|e| e.to_string()))
    }

    pub(crate) fn kv_get(namespace: &str, key: &str) -> KvResult<String> {
        with_conn(|conn| {
            let mut stmt = conn.prepare_cached("SELECT value FROM kv_store WHERE ns = ? AND key = ?")?;
            let value: Option<String> = stmt.query_row(rusqlite::params![namespace, key], |row| row.get(0)).optional()?;
            Ok(value.unwrap_or_default())
        })
    }

    pub(crate) fn kv_delete(namespace: &str, key: &str) -> KvResult<()> {
        with_conn(|conn| {
            conn.prepare_cached("DELETE FROM kv_store WHERE ns = ? AND key = ?")?
                .execute(rusqlite::params![namespace, key])
                .map(drop)
        })
    }

    pub(crate) fn kv_clear(namespace: &str) -> KvResult<()> {
        with_conn(|conn| {
            conn.prepare_cached("DELETE FROM kv_store WHERE ns = ?")?.execute(rusqlite::params![namespace]).map(drop)
        })
    }

    pub(crate) fn kv_put(namespace: &str, key: &str, value: &str) -> KvResult<()> {
        with_conn(|conn| {
            conn.prepare_cached("INSERT OR REPLACE INTO kv_store (ns, key, value) VALUES (?, ?, ?)")?
                .execute(rusqlite::params![namespace, key, value])
                .map(drop)
        })
    }

    /// Namespace and key are separate columns, so `kv_clear` matches the namespace exactly.
    /// Rows of the legacy `kv (key = "<ns>.<key>")` table are copied once, split at the first '.';
    /// the legacy table itself is left untouched.
    fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
        let exists = |table: &str| -> rusqlite::Result<bool> {
            conn.query_row("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?", [table], |row| {
                row.get::<_, i64>(0)
            }).map(|n| n > 0)
        };
        if exists("kv_store")? {
            return Ok(());
        }
        let legacy = exists("kv")?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "CREATE TABLE kv_store (ns TEXT NOT NULL, key TEXT NOT NULL, value TEXT, PRIMARY KEY (ns, key))",
            [],
        )?;
        if legacy {
            tx.execute(
                "INSERT OR IGNORE INTO kv_store (ns, key, value) \
                 SELECT substr(key, 1, instr(key, '.') - 1), substr(key, instr(key, '.') + 1), value \
                 FROM kv WHERE instr(key, '.') > 0",
                [],
            )?;
        }
        tx.commit()
    }

    fn create_sqlite_kv_conn() -> KvResult<Connection> {
        let awk_config_dir = dirs::home_dir().ok_or("cannot find the home directory")?.join(".awk");
        std::fs::create_dir_all(&awk_config_dir)
            .map_err(|e| format!("cannot create {}: {}", awk_config_dir.display(), e))?;
        let sqlite_kv_db = awk_config_dir.join("sqlite.db");
        let open = || -> rusqlite::Result<Connection> {
            let conn = Connection::open(sqlite_kv_db.as_path())?;
            conn.set_prepared_statement_cache_capacity(128);
            init_schema(&conn)?;
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
    fn test_redis_operations() {
        let namespace = "redis://127.0.0.1:6379/demo1";
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
        assert_eq!(kv_get("rediss://127.0.0.1:1/demo", "nick"), "");
        assert_eq!(kv_get("redis+tls://127.0.0.1:1/demo", "nick"), "");
    }

    #[test]
    fn test_namespace_routing() {
        for url in ["redis://h/x", "rediss://h/x", "redis+tls://h/x"] {
            assert!(is_redis_url(url), "{}", url);
        }
        assert!(unknown_scheme("redis+ssl://h/x").is_some());
        assert!(unknown_scheme("mongodb://h/x").is_some());
        assert!(unknown_scheme("demo").is_none());
        assert!(unknown_scheme("a b://c").is_none());
        // an unknown scheme is not written to the local SQLite database
        kv_put("zawk-unknown://h/x", "k", "v");
        assert_eq!(sqlite_kv::kv_get("zawk-unknown://h/x", "k").unwrap(), "");
    }

    #[test]
    #[ignore]
    fn test_nats_get() {
        let value = "Jackie";
        let url = "nats://localhost:4222/bucket2";
        nats_kv::kv_put(url, "nick", value).unwrap();
        assert_eq!(nats_kv::kv_get(url, "nick").unwrap(), value);
        nats_kv::kv_put(url, "phone", "138xxx").unwrap();
        nats_kv::kv_delete(url, "nick").unwrap();
        assert_eq!(nats_kv::kv_get(url, "nick").unwrap(), "");
        nats_kv::kv_clear(url).unwrap();
        assert_eq!(nats_kv::kv_get(url, "phone").unwrap(), "");
    }

    #[test]
    fn test_sqlite_clear_is_exact() {
        sqlite_kv::kv_put("zawk_t.demo", "k", "1").unwrap();
        sqlite_kv::kv_put("zawk_t_dxmo", "k", "2").unwrap();
        sqlite_kv::kv_put("zawk_t", "demo.k", "3").unwrap();
        sqlite_kv::kv_clear("zawk_t_d_mo").unwrap();
        sqlite_kv::kv_clear("ZAWK_T.DEMO").unwrap();
        assert_eq!(sqlite_kv::kv_get("zawk_t.demo", "k").unwrap(), "1");
        assert_eq!(sqlite_kv::kv_get("zawk_t_dxmo", "k").unwrap(), "2");
        assert_eq!(sqlite_kv::kv_get("zawk_t", "demo.k").unwrap(), "3");
        sqlite_kv::kv_clear("zawk_t.demo").unwrap();
        assert_eq!(sqlite_kv::kv_get("zawk_t.demo", "k").unwrap(), "");
        assert_eq!(sqlite_kv::kv_get("zawk_t", "demo.k").unwrap(), "3");
        for ns in ["zawk_t_dxmo", "zawk_t"] {
            sqlite_kv::kv_clear(ns).unwrap();
        }
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
