use crate::runtime::conn_pool::ConnSlots;
use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::float_to_text;
use crate::runtime::str_escape::{is_null, null_input};
use crate::runtime::{Int, IntMap, Str, stdlib_warning};
use rusqlite::types::Value;
use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::{Batch, Connection, params_from_iter};
use std::sync::LazyLock;
use std::time::Duration;

/// Connections by database and, for a database file, by thread (see [`slot_key`]).
static SQLITE_CONNECTIONS: LazyLock<ConnSlots<Connection>> = LazyLock::new(ConnSlots::new);

/// The separator of the database and the thread in a key of [`SQLITE_CONNECTIONS`].
const THREAD_SEPARATOR: char = '\0';

/// Whether every connection to `db_path` opens a separate database (in memory or temporary).
fn is_private_database(db_path: &str) -> bool {
    db_path.is_empty()
        || db_path == ":memory:"
        || db_path.starts_with("file::memory:")
        || db_path.contains("mode=memory")
}

/// The connection cache key of `db_path`. A database file has a connection per thread, so a
/// transaction opened by `BEGIN` in one call, and its writes, stay with the worker that opened it,
/// as for MySQL and PostgreSQL: with one shared connection, a `ROLLBACK` of one worker of a `-p`
/// run would undo the writes of the others, and its `COMMIT` commit them half done. Writers wait
/// for each other's transactions (up to the busy timeout). A database in memory is a single
/// connection shared by all threads, since another connection would be another database.
fn slot_key(db_path: &str) -> String {
    if is_private_database(db_path) {
        db_path.to_string()
    } else {
        format!("{}{}{:?}", db_path, THREAD_SEPARATOR, std::thread::current().id())
    }
}

/// Warn about each database left inside a transaction (a `BEGIN` without `COMMIT`): cached
/// connections are not closed at exit, and SQLite rolls such a transaction back when the
/// database is next opened, so its writes would otherwise be lost silently.
pub(crate) fn warn_open_transactions() {
    let mut open = vec![];
    SQLITE_CONNECTIONS.for_each(|key, conn| {
        if !conn.is_autocommit() {
            let db_path = key.split(THREAD_SEPARATOR).next().unwrap_or(key);
            open.push(db_path.to_string());
        }
    });
    open.sort();
    open.dedup();
    for db_path in open {
        stdlib_warning(
            "sqlite_execute",
            format!(
                "{}: a transaction was never committed (BEGIN without COMMIT): its changes are rolled back",
                db_path
            ),
        );
    }
}

/// How long a statement waits for a lock held by another connection before failing with
/// `database is locked`.
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Opens `db_path` with a busy timeout, so concurrent writers wait instead of failing at once.
pub(crate) fn open_connection<P: AsRef<std::path::Path>>(
    db_path: P,
) -> rusqlite::Result<Connection> {
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
    if is_null(text.as_bytes()) {
        return Value::Null;
    }
    if let Ok(n) = text.parse::<i64>() {
        if n.to_string() == text {
            return Value::Integer(n);
        }
    } else if let Ok(f) = text.parse::<f64>()
        && f.is_finite() && float_to_text(f) == text {
            return Value::Real(f);
        }
    Value::Text(text.to_owned())
}

fn sqlite_params(params: &IntMap<Str>) -> Vec<Value> {
    positional_params(params)
        .iter()
        .map(|s| sqlite_param(s))
        .collect()
}

pub(crate) fn sqlite_query<'a>(db_path: &str, sql: &str, params: &IntMap<Str>) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if let Err(e) = sqlite_query_into(db_path, sql, &sqlite_params(params), &map) {
        stdlib_warning("sqlite_query", e);
    }
    map
}

fn sqlite_query_into(
    db_path: &str,
    sql: &str,
    params: &[Value],
    map: &IntMap<Str>,
) -> rusqlite::Result<()> {
    SQLITE_CONNECTIONS.with_conn(
        &slot_key(db_path),
        || open_connection(db_path),
        |conn| query_into(conn, sql, params, map),
    )
}

fn query_into(
    conn: &Connection,
    sql: &str,
    params: &[Value],
    map: &IntMap<Str>,
) -> rusqlite::Result<()> {
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
                Value::Null => null_input().to_owned(),
                Value::Integer(num) => num.to_string(),
                Value::Real(num) => float_to_text(num),
                Value::Text(text) => text.to_string(),
                Value::Blob(_) => "".to_owned(),
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

/// Runs `sql` and returns the number of rows it changed. With parameters `sql` is a single
/// statement. Without, it may hold several statements separated by `;` (e.g. `BEGIN; ...;
/// COMMIT`): they run in order, each prepared only after the previous one ran (so a table
/// created by one can be used by the next), and the rows changed by all of them are added up.
/// The first failing statement stops the run; the statements before it stay applied, except
/// when they opened a transaction: it is rolled back, so `BEGIN; ...; COMMIT` applies all of its
/// statements or none (left open, it would take in the writes of the later calls, all lost at
/// exit). A transaction opened by an earlier call is left to the program.
fn execute(conn: &Connection, sql: &str, params: &[Value]) -> rusqlite::Result<usize> {
    if !params.is_empty() {
        return conn.execute(sql, params_from_iter(params));
    }
    let autocommit = conn.is_autocommit();
    let result = execute_batch(conn, sql);
    if result.is_err() && autocommit && !conn.is_autocommit() {
        conn.execute_batch("ROLLBACK")?;
    }
    result
}

fn execute_batch(conn: &Connection, sql: &str) -> rusqlite::Result<usize> {
    let mut batch = Batch::new(conn, sql);
    let mut changed = 0;
    while let Some(mut stmt) = batch.next()? {
        let before = conn.total_changes();
        let n = stmt.execute([])?;
        // `changes()` keeps the count of the last INSERT/UPDATE/DELETE: only count it when this
        // statement changed rows, not for DDL, BEGIN/COMMIT and the like
        if conn.total_changes() != before {
            changed += n;
        }
    }
    Ok(changed)
}

pub(crate) fn sqlite_execute(db_path: &str, sql: &str, params: &IntMap<Str>) -> Int {
    let params = sqlite_params(params);
    let result = SQLITE_CONNECTIONS.with_conn(
        &slot_key(db_path),
        || open_connection(db_path),
        |conn| execute(conn, sql, &params),
    );
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
    fn test_execute_multiple_statements() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("multi.db");
        let db_path = db_path.to_str().unwrap();
        let none = IntMap::default();
        // DDL, a transaction and several inserts in one call: the changed rows are added up
        let sql = "CREATE TABLE t (x INT); BEGIN; INSERT INTO t VALUES (1); \
                   INSERT INTO t VALUES (2), (3); COMMIT;";
        assert_eq!(sqlite_execute(db_path, sql, &none), 3);
        let count = |db: &str| sqlite_query(db, "SELECT count(*) FROM t", &none).get(&1);
        assert_eq!(count(db_path).as_str(), "3");
        // DDL alone does not report the count of an earlier INSERT
        assert_eq!(
            sqlite_execute(db_path, "CREATE INDEX i ON t (x); ", &none),
            0
        );
        // a failing statement stops the run, the earlier ones stay applied
        let sql = "INSERT INTO t VALUES (4); INSERT INTO missing VALUES (5); INSERT INTO t VALUES (6)";
        assert_eq!(sqlite_execute(db_path, sql, &none), 0);
        assert_eq!(count(db_path).as_str(), "4");
        // a placeholder without a value is still an error
        assert_eq!(sqlite_execute(db_path, "INSERT INTO t VALUES (?)", &none), 0);
        assert_eq!(count(db_path).as_str(), "4");
    }

    #[test]
    fn test_failed_batch_rolls_back_its_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("tx.db");
        let db_path = db_path.to_str().unwrap();
        let none = IntMap::default();
        let count = |db: &str| sqlite_query(db, "SELECT count(*) FROM t", &none).get(&1);
        assert_eq!(sqlite_execute(db_path, "CREATE TABLE t (x INT)", &none), 0);
        let sql = "BEGIN; INSERT INTO t VALUES (1); INSERT INTO missing VALUES (2); COMMIT";
        assert_eq!(sqlite_execute(db_path, sql, &none), 0);
        // the whole batch is undone, and the next write is not caught in an open transaction
        assert_eq!(count(db_path).as_str(), "0");
        assert_eq!(sqlite_execute(db_path, "INSERT INTO t VALUES (3)", &none), 1);
        // a new connection, as the next run of a program, sees the write
        let other = open_connection(db_path).unwrap();
        let n: i64 = other
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 1);
        drop(other);

        // a transaction opened by an earlier call is left to the program
        assert_eq!(sqlite_execute(db_path, "BEGIN", &none), 0);
        assert_eq!(sqlite_execute(db_path, "INSERT INTO t VALUES (4)", &none), 1);
        let sql = "INSERT INTO t VALUES (5); INSERT INTO missing VALUES (6)";
        assert_eq!(sqlite_execute(db_path, sql, &none), 0);
        assert_eq!(count(db_path).as_str(), "3");
        assert_eq!(sqlite_execute(db_path, "ROLLBACK", &none), 0);
        assert_eq!(count(db_path).as_str(), "1");
    }

    #[test]
    fn test_file_connections_per_thread() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("threads.db");
        let db_path = db_path.to_str().unwrap().to_string();
        let none = IntMap::default();
        let count = |db: &str| sqlite_query(db, "SELECT count(*) FROM t", &IntMap::default()).get(&1).to_string();
        assert_eq!(sqlite_execute(&db_path, "CREATE TABLE t (x INT)", &none), 0);
        assert_eq!(sqlite_execute(&db_path, "BEGIN", &none), 0);
        assert_eq!(sqlite_execute(&db_path, "INSERT INTO t VALUES (1)", &none), 1);
        // another thread has its own connection: it does not see the uncommitted row, and its
        // write is not part of this thread's transaction (it waits for it, then fails here)
        let path = db_path.clone();
        let seen = std::thread::spawn(move || count(&path)).join().unwrap();
        assert_eq!(seen, "0");
        assert_eq!(sqlite_execute(&db_path, "ROLLBACK", &none), 0);
        let path = db_path.clone();
        let written = std::thread::spawn(move || {
            sqlite_execute(&path, "INSERT INTO t VALUES (2)", &IntMap::default())
        })
        .join()
        .unwrap();
        assert_eq!(written, 1);
        assert_eq!(count(&db_path), "1");
        // `:memory:` stays one database for all threads
        assert_eq!(sqlite_execute(":memory:", "CREATE TABLE IF NOT EXISTS m (x INT)", &none), 0);
        let rows = std::thread::spawn(|| {
            sqlite_query(":memory:", "SELECT count(*) FROM m", &IntMap::default()).len()
        })
        .join()
        .unwrap();
        assert_eq!(rows, 1);
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
        assert_eq!(
            sqlite_execute(
                db_path,
                insert,
                &params(&["jackie", "jackie@example.com", "30"])
            ),
            1
        );
        assert_eq!(
            sqlite_execute(
                db_path,
                insert,
                &params(&["linux", "linux@example.com", "42"])
            ),
            1
        );
        // the UNIQUE constraint fails: a warning, no row inserted
        assert_eq!(
            sqlite_execute(db_path, insert, &params(&["linux", "dup@example.com", "1"])),
            0
        );

        let rows = sqlite_query(
            db_path,
            "SELECT nick, email, age FROM user ORDER BY age",
            &none,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.get(&1).to_string(), "jackie,jackie@example.com,30");
        assert_eq!(rows.get(&2).to_string(), "linux,linux@example.com,42");

        let rows = sqlite_query(
            db_path,
            "SELECT nick FROM user WHERE age > ?",
            &params(&["35"]),
        );
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
        for text in [
            "007", "+5", "5.0", "1e3", ".5", "inf", "nan", "abc", "", " 5",
        ] {
            assert_eq!(sqlite_param(text), Value::Text(text.to_owned()), "{text}");
        }

        let rows = sqlite_query(
            ":memory:",
            "SELECT ? > 10, typeof(?1), typeof(?2), ?2 < 2",
            &params(&["5", "1.5"]),
        );
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
        holder
            .execute_batch("CREATE TABLE t(v INTEGER); BEGIN IMMEDIATE; INSERT INTO t VALUES (1);")
            .unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            holder.execute_batch("COMMIT").unwrap();
        });

        // Without a busy timeout this fails at once with `database is locked`.
        let writer = open_connection(&db_path).unwrap();
        assert_eq!(writer.execute("INSERT INTO t VALUES (2)", []).unwrap(), 1);
        releaser.join().unwrap();
        let count: i64 = writer
            .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
