use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use rusqlite::{params_from_iter, Connection};
use rusqlite::types::{Value};
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;

static SQLITE_CONNECTIONS: LazyLock<Arc<Mutex<HashMap<String, rusqlite::Connection>>>> = LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Values of `params` ordered by key: the positional parameters (`?`, `?1`, `$1`...) bound to a
/// prepared statement by the `*_query`/`*_execute` database functions.
pub(crate) fn positional_params(params: &IntMap<Str>) -> Vec<String> {
    let mut keys = params.to_vec();
    keys.sort_unstable();
    keys.iter().map(|k| params.get(k).to_string()).collect()
}

pub(crate) fn sqlite_query<'a>(db_path: &str, sql: &str, params: &IntMap<Str>) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if let Err(e) = sqlite_query_into(db_path, sql, &positional_params(params), &map) {
        stdlib_warning("sqlite_query", e);
    }
    map
}

fn sqlite_query_into(db_path: &str, sql: &str, params: &[String], map: &IntMap<Str>) -> rusqlite::Result<()> {
    let mut pool = SQLITE_CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let conn = open_connection(&mut pool, db_path)?;
    let mut stmt = conn.prepare_cached(sql)?;
    let colum_count = stmt.column_count();
    let mut index = 1;
    let mut rows = stmt.query(params_from_iter(params))?;
    while let Some(row) = rows.next()? {
        let mut items: Vec<String> = vec![];
        let mut i = 0;
        while i < colum_count {
            let value = row.get::<_, Value>(i)?;
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
    Ok(())
}

fn open_connection<'p>(
    pool: &'p mut HashMap<String, Connection>,
    db_path: &str,
) -> rusqlite::Result<&'p mut Connection> {
    if !pool.contains_key(db_path) {
        pool.insert(db_path.to_string(), Connection::open(db_path)?);
    }
    Ok(pool.get_mut(db_path).unwrap())
}

pub(crate) fn sqlite_execute(db_path: &str, sql: &str, params: &IntMap<Str>) -> Int {
    let params = positional_params(params);
    let mut pool = SQLITE_CONNECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let result = open_connection(&mut pool, db_path).and_then(|conn| conn.execute(sql, params_from_iter(&params)));
    match result {
        Ok(n) => n as Int,
        Err(e) => {
            stdlib_warning("sqlite_execute", e);
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query() {
        let sql = "SELECT nick, email, age FROM user";
        let db_path = "sqlite.db";
        let rows = sqlite_query(db_path, sql, &IntMap::default());
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    fn test_create_db() {
        let sql = "CREATE TABLE IF NOT EXISTS user (nick VARCHAR UNIQUE, email VARCHAR, age INT)";
        let db_path = "sqlite.db";
        let _ = sqlite_execute(db_path, sql, &IntMap::default());
    }
}
