use std::sync::LazyLock;
use rusqlite::{params_from_iter, Connection};
use rusqlite::types::{Value};
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::conn_pool::ConnSlots;
use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::float_to_text;

static SQLITE_CONNECTIONS: LazyLock<ConnSlots<Connection>> = LazyLock::new(ConnSlots::new);

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
    SQLITE_CONNECTIONS.with_conn(db_path, || Connection::open(db_path), |conn| query_into(conn, sql, params, map))
}

fn query_into(conn: &Connection, sql: &str, params: &[String], map: &IntMap<Str>) -> rusqlite::Result<()> {
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
                Value::Real(num) => { float_to_text(num) }
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

pub(crate) fn sqlite_execute(db_path: &str, sql: &str, params: &IntMap<Str>) -> Int {
    let params = positional_params(params);
    let result = SQLITE_CONNECTIONS.with_conn(db_path, || Connection::open(db_path), |conn| {
        conn.execute(sql, params_from_iter(&params))
    });
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

    fn params(values: &[&str]) -> IntMap<Str<'static>> {
        let map = IntMap::default();
        for (i, v) in values.iter().enumerate() {
            map.insert(i as Int + 1, Str::from(v.to_string()));
        }
        map
    }

    #[test]
    fn test_execute_and_query() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_path = db_path.to_str().unwrap();
        let none = IntMap::default();

        let sql = "CREATE TABLE IF NOT EXISTS user (nick VARCHAR UNIQUE, email VARCHAR, age INT)";
        assert_eq!(sqlite_execute(db_path, sql, &none), 0);
        let insert = "INSERT INTO user (nick, email, age) VALUES (?, ?, ?)";
        assert_eq!(sqlite_execute(db_path, insert, &params(&["jackie", "jackie@example.com", "30"])), 1);
        assert_eq!(sqlite_execute(db_path, insert, &params(&["linux", "linux@example.com", "42"])), 1);
        // the UNIQUE constraint fails: a warning, no row inserted
        assert_eq!(sqlite_execute(db_path, insert, &params(&["linux", "dup@example.com", "1"])), 0);

        let rows = sqlite_query(db_path, "SELECT nick, email, age FROM user ORDER BY age", &none);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.get(&1).to_string(), "jackie,jackie@example.com,30");
        assert_eq!(rows.get(&2).to_string(), "linux,linux@example.com,42");

        let rows = sqlite_query(db_path, "SELECT nick FROM user WHERE age > ?", &params(&["35"]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows.get(&1).to_string(), "linux");

        let rows = sqlite_query(db_path, "SELECT nick FROM no_such_table", &none);
        assert_eq!(rows.len(), 0);
    }
}
