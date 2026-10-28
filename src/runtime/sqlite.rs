use std::sync::LazyLock;
use std::time::Duration;
use rusqlite::{params_from_iter, Connection};
use rusqlite::types::{Value};
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::conn_pool::ConnSlots;
use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::float_to_text;

static SQLITE_CONNECTIONS: LazyLock<ConnSlots<Connection>> = LazyLock::new(ConnSlots::new);

/// How long a statement waits for a lock held by another connection before failing with
/// `database is locked`.
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens `db_path` with a busy timeout, so concurrent writers wait instead of failing at once.
pub(crate) fn open_connection<P: AsRef<std::path::Path>>(db_path: P) -> rusqlite::Result<Connection> {
    let conn = Connection::open(db_path)?;
    conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
    Ok(conn)
}

/// Values of `params` ordered by key: the positional parameters (`?`, `?1`, `$1`...) bound to a
/// prepared statement by the `*_query`/`*_execute` database functions.
pub(crate) fn positional_params(params: &IntMap<Str>) -> Vec<String> {
    let mut keys = params.to_vec();
    keys.sort_unstable();
    keys.iter().map(|k| params.get(k).to_string()).collect()
}

/// Binds `text` as an INTEGER/REAL when it is exactly the canonical text of that number, so `? > 10`
/// and columns without affinity compare numerically. Anything else (`007`, `1e3`, `5.0`) stays TEXT.
fn sqlite_param(text: &str) -> Value {
    if let Ok(n) = text.parse::<i64>() {
        if n.to_string() == text {
            return Value::Integer(n);
        }
    } else if let Ok(f) = text.parse::<f64>() {
        if f.is_finite() && float_to_text(f) == text {
            return Value::Real(f);
        }
    }
    Value::Text(text.to_owned())
}

fn sqlite_params(params: &IntMap<Str>) -> Vec<Value> {
    positional_params(params).iter().map(|s| sqlite_param(s)).collect()
}

pub(crate) fn sqlite_query<'a>(db_path: &str, sql: &str, params: &IntMap<Str>) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if let Err(e) = sqlite_query_into(db_path, sql, &sqlite_params(params), &map) {
        stdlib_warning("sqlite_query", e);
    }
    map
}

fn sqlite_query_into(db_path: &str, sql: &str, params: &[Value], map: &IntMap<Str>) -> rusqlite::Result<()> {
    SQLITE_CONNECTIONS.with_conn(db_path, || open_connection(db_path), |conn| query_into(conn, sql, params, map))
}

fn query_into(conn: &Connection, sql: &str, params: &[Value], map: &IntMap<Str>) -> rusqlite::Result<()> {
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
    let params = sqlite_params(params);
    let result = SQLITE_CONNECTIONS.with_conn(db_path, || open_connection(db_path), |conn| {
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

    #[test]
    fn test_numeric_params() {
        assert_eq!(sqlite_param("5"), Value::Integer(5));
        assert_eq!(sqlite_param("-12"), Value::Integer(-12));
        assert_eq!(sqlite_param("1.5"), Value::Real(1.5));
        for text in ["007", "+5", "5.0", "1e3", ".5", "inf", "nan", "abc", "", " 5"] {
            assert_eq!(sqlite_param(text), Value::Text(text.to_owned()), "{text}");
        }

        let rows = sqlite_query(":memory:", "SELECT ? > 10, typeof(?1), typeof(?2), ?2 < 2", &params(&["5", "1.5"]));
        assert_eq!(rows.get(&1).to_string(), "0,integer,real,1");
        let rows = sqlite_query(":memory:", "SELECT typeof(?)", &params(&["007"]));
        assert_eq!(rows.get(&1).to_string(), "text");
    }

    #[test]
    fn test_busy_timeout_waits_for_lock() {
        let dir = std::env::temp_dir().join(format!("zawk-sqlite-busy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("busy.db");
        let _ = std::fs::remove_file(&db_path);

        let holder = open_connection(&db_path).unwrap();
        holder.execute_batch("CREATE TABLE t(v INTEGER); BEGIN IMMEDIATE; INSERT INTO t VALUES (1);").unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            holder.execute_batch("COMMIT").unwrap();
        });

        // Without a busy timeout this fails at once with `database is locked`.
        let writer = open_connection(&db_path).unwrap();
        assert_eq!(writer.execute("INSERT INTO t VALUES (2)", []).unwrap(), 1);
        releaser.join().unwrap();
        let count: i64 = writer.query_row("SELECT count(*) FROM t", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
