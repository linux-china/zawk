use std::collections::HashMap;
use std::sync::Mutex;
use lazy_static::lazy_static;
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use libsql::{Builder, params, Value};

lazy_static! {
    static ref LIBSQL_CONNECTIONS: Mutex<HashMap<String, libsql::Connection>> = Mutex::new(HashMap::new());
}

pub(crate) async fn libsql_query<'a>(db_path: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pool = LIBSQL_CONNECTIONS.lock().unwrap();
    if !pool.contains_key(db_path) {
        let mut url = db_path.to_string();
        let mut auth_token = "".to_string();
        if db_path.contains('?') {
            let offset = db_path.find('?').unwrap();
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
        let connection = Builder::new_remote(url, auth_token).build().await.unwrap().connect().unwrap();
        pool.insert(db_path.to_string(), connection);
    }
    let conn = pool.get(db_path).unwrap();
    let mut stmt = conn.prepare(sql).await.unwrap();
    let mut index = 1;
    let mut colum_count = 0;
    let mut rows = stmt.query(params![]).await.unwrap();
    while let Some(row) = rows.next().await.unwrap() {
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
    map
}

pub(crate) async fn libsql_execute(db_path: &str, sql: &str) -> Int {
    let mut pool = LIBSQL_CONNECTIONS.lock().unwrap();
    if !pool.contains_key(db_path) {
        let connection = Builder::new_remote(db_path.to_string(), "".to_string()).build().await.unwrap().connect().unwrap();
        pool.insert(db_path.to_string(), connection);
    }
    let conn = pool.get(db_path).unwrap();
    conn.execute(sql, params![]).await.unwrap_or(0) as Int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_query() {
        let sql = "SELECT id, email FROM users";
        let db_path = "http://127.0.0.1:8080";
        let rows = libsql_query(db_path, sql).await;
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[tokio::test]
    async fn test_create_db() {
        let sql = "CREATE TABLE IF NOT EXISTS user (nick VARCHAR UNIQUE, email VARCHAR, age INT)";
        let db_path = "http://127.0.0.1:8080";
        let _ = libsql_execute(db_path, sql).await;
    }
}