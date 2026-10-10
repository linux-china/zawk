use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use libsql::{Builder, params, Value};

lazy_static! {
    static ref LIBSQL_CONNECTIONS: Arc<Mutex<HashMap<String, libsql::Connection>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub(crate) fn libsql_query<'a>(db_path: &str, sql: &str) -> IntMap<Str<'a>> {
    crate::runtime::TOKIO_RUNTIME.block_on(async {
        libsql_query_async(db_path, sql).await
    })
}

pub(crate) async fn libsql_query_async<'a>(db_path: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pool = LIBSQL_CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    if !pool.contains_key(db_path) {
        let (url, auth_token) = remote_url(db_path);
        if !(url.starts_with("libsql://")
            || url.starts_with("http://")
            || url.starts_with("https://")) {
            // local database: use rusqlite to avoid linking a second bundled SQLite
            drop(pool);
            return crate::runtime::sqlite::sqlite_query(url.as_str(), sql);
        }
        match connect(url, auth_token).await {
            Ok(connection) => {
                pool.insert(db_path.to_string(), connection);
            }
            Err(e) => {
                stdlib_warning("libsql_query", e);
                return map;
            }
        }
    }
    let conn = &pool[db_path];
    if let Err(e) = libsql_query_into(conn, sql, &map).await {
        stdlib_warning("libsql_query", e);
    }
    map
}

fn remote_url(db_path: &str) -> (String, String) {
    let mut url = db_path.to_string();
    let mut auth_token = env::var("LIBSQL_AUTH_TOKEN").unwrap_or("".to_owned());
    if let Some(offset) = db_path.find('?') {
        url = db_path[0..offset].to_string();
        if let Some(pos) = db_path.find("authToken=") {
            auth_token = db_path[pos + 10..].to_string();
        } else {
            auth_token = db_path[offset + 1..].to_string();
        }
    }
    if url.starts_with("ws://") {
        url = url.replace("ws://", "http://").to_string();
    } else if url.starts_with("wss://") {
        url = url.replace("wss://", "https://").to_string();
    }
    (url, auth_token)
}

async fn connect(url: String, auth_token: String) -> libsql::Result<libsql::Connection> {
    Builder::new_remote(url, auth_token).build().await?.connect()
}

async fn libsql_query_into(conn: &libsql::Connection, sql: &str, map: &IntMap<Str<'_>>) -> libsql::Result<()> {
    let stmt = conn.prepare(sql).await?;
    let mut index = 1;
    let mut colum_count = 0;
    let mut rows = stmt.query(params![]).await?;
    while let Some(row) = rows.next().await? {
        let mut items: Vec<String> = vec![];
        let mut i: i32 = 0;
        if colum_count == 0 {
            let text = format!("{:?}", row);
            colum_count = text.match_indices("Col {").count() as i32;
        }
        while i < colum_count {
            if let Ok(value) = row.get_value(i) {
                let text_value = match value {
                    Value::Null => { "".to_owned() }
                    Value::Integer(num) => { num.to_string() }
                    Value::Real(num) => { num.to_string() }
                    Value::Text(text) => { text.to_string() }
                    Value::Blob(_) => { "".to_owned() }
                };
                items.push(text_value);
            }
            i += 1;
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
        index += 1;
    }
    Ok(())
}
pub(crate) fn libsql_execute(db_path: &str, sql: &str) -> Int {
    crate::runtime::TOKIO_RUNTIME.block_on(async {
        libsql_execute_async(db_path, sql).await
    })
}

pub(crate) async fn libsql_execute_async(db_path: &str, sql: &str) -> Int {
    let mut pool = LIBSQL_CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    if !pool.contains_key(db_path) {
        match connect(db_path.to_string(), "".to_string()).await {
            Ok(connection) => {
                pool.insert(db_path.to_string(), connection);
            }
            Err(e) => {
                stdlib_warning("libsql_execute", e);
                return 0;
            }
        }
    }
    match pool[db_path].execute(sql, params![]).await {
        Ok(n) => n as Int,
        Err(e) => {
            stdlib_warning("libsql_execute", e);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn test_query_async() {
        let sql = "SELECT id, email FROM users";
        let db_path = "http://127.0.0.1:8080";
        let rows = libsql_query_async(db_path, sql).await;
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    #[ignore]
    fn test_query() {
        let sql = "SELECT id, email FROM users";
        let db_path = "http://127.0.0.1:8080";
        let rows = libsql_query(db_path, sql);
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[tokio::test]
    #[ignore]
    async fn test_create_db() {
        let sql = "CREATE TABLE IF NOT EXISTS user (nick VARCHAR UNIQUE, email VARCHAR, age INT)";
        let db_path = "http://127.0.0.1:8080";
        let _ = libsql_execute(db_path, sql);
    }
}
