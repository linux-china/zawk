use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use rusqlite::{params, Connection};
use rusqlite::types::{Value};
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;

lazy_static! {
    static ref SQLITE_CONNECTIONS: Arc<Mutex<HashMap<String, rusqlite::Connection>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub(crate) fn sqlite_query<'a>(db_path: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(db_path.to_string()).or_insert_with(|| {
        Connection::open(db_path).unwrap()
    });
    let mut stmt = conn.prepare(sql).unwrap();
    let colum_count = stmt.column_count();
    let mut index = 1;
    let mut rows = stmt.query(params![]).unwrap();
    while let Some(row) = rows.next().unwrap() {
        let mut items: Vec<String> = vec![];
        let mut i = 0;
        while i < colum_count {
            let value = row.get::<_, Value>(i).unwrap();
            let text_value = match value {
                Value::Null => { "".to_owned() }
                Value::Integer(num) => { num.to_string() }
                Value::Real(num) => { num.to_string() }
                Value::Text(text) => { text.to_string() }
                Value::Blob(_) => { "".to_owned() }
            };
            items.push(text_value);
            i += 1;
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
        index += 1;
    }
    map
}

pub(crate) fn sqlite_execute(db_path: &str, sql: &str) -> Int {
    let mut pool = SQLITE_CONNECTIONS.lock().unwrap();
    let conn = pool.entry(db_path.to_string()).or_insert_with(|| {
        Connection::open(db_path).unwrap()
    });
    conn.execute(sql, rusqlite::params![]).unwrap_or(0) as Int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query() {
        let sql = "SELECT nick, email, age FROM user";
        let db_path = "sqlite.db";
        let rows = sqlite_query(db_path, sql);
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    fn test_create_db() {
        let sql = "CREATE TABLE IF NOT EXISTS user (nick VARCHAR UNIQUE, email VARCHAR, age INT)";
        let db_path = "sqlite.db";
        let _ = sqlite_execute(db_path, sql);
    }
}
