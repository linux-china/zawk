use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::{f32_to_text, float_to_text};
use crate::runtime::str_escape::{is_null, null_input};
use crate::runtime::{Int, IntMap, Str, stdlib_warning};
use mysql::consts::ColumnType;
use mysql::prelude::*;
use mysql::*;
use crate::runtime::conn_pool::{Dialect, Driver, SessionPool, is_read_only_sql};
use std::sync::LazyLock;

/// Connections by URL and thread. `mysql::Pool` is not used: it resets a connection when it is
/// returned (rolling back a `BEGIN` from an earlier call) and hands it to any thread.
static MYSQL_CONNECTIONS: LazyLock<SessionPool<Conn>> =
    LazyLock::new(|| SessionPool::new(Dialect::MySql));

/// Warn about each URL left inside a transaction (a `BEGIN` without `COMMIT`): the server rolls
/// it back when the connection is closed at exit, so its writes would otherwise be lost silently.
pub(crate) fn warn_open_transactions() {
    for url in MYSQL_CONNECTIONS.open_transactions() {
        crate::runtime::warn_open_transaction("mysql_execute", &url);
    }
}

/// Positional parameters bound as strings (MySQL converts them to the column types), the `--null`
/// text as NULL, or `None` when there are none: then the text protocol is used, since not every
/// statement can be prepared.
fn positional_params(params: &IntMap<Str>) -> Option<Params> {
    let values = crate::runtime::sqlite::positional_params(params);
    if values.is_empty() {
        None
    } else {
        let value = |s: String| {
            if is_null(s.as_bytes()) {
                Value::NULL
            } else {
                Value::from(s)
            }
        };
        Some(Params::Positional(values.into_iter().map(value).collect()))
    }
}

pub(crate) fn mysql_query<'a>(db_url: &str, sql: &str, params: &IntMap<Str>) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    // only a plain read is replayed after the connection was lost: a write (`CALL`, `INSERT ...;
    // SELECT LAST_INSERT_ID()`) may already have been applied
    let retry = is_read_only_sql(sql, Dialect::MySql);
    let result = match positional_params(params) {
        Some(params) => with_conn(db_url, sql, retry, |conn| conn.exec(sql, params.clone())),
        None => with_conn(db_url, sql, retry, |conn| conn.query(sql)),
    };
    let rows: Vec<Row> = match result {
        Ok(rows) => rows,
        Err(e) => {
            stdlib_warning("mysql_query", e);
            return map;
        }
    };
    for (index, row) in (1..).zip(rows) {
        let mut items: Vec<String> = vec![];
        for i in 0..row.len() {
            let col_value: Value = row.get(i).unwrap_or(Value::NULL);
            let text_value = match col_value {
                Value::NULL => null_input().to_owned(),
                Value::Bytes(bytes) => bytes_text(&row.columns_ref()[i], &bytes),
                Value::Int(num) => num.to_string(),
                Value::UInt(num) => num.to_string(),
                Value::Float(num) => f32_to_text(num),
                Value::Double(num) => float_to_text(num),
                Value::Date(year, month, day, hour, minutes, seconds, micro_seconds) => {
                    let date_only =
                        row.columns_ref()[i].column_type() == ColumnType::MYSQL_TYPE_DATE;
                    format_mysql_date(
                        (year, month, day),
                        (hour, minutes, seconds, micro_seconds),
                        date_only,
                    )
                }
                Value::Time(negative, days, hours, minutes, seconds, micro_seconds) => {
                    format_mysql_time(negative, days, hours, minutes, seconds, micro_seconds)
                }
            };
            items.push(text_value);
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
    }
    map
}

/// The character set number of binary strings (`BINARY`, `VARBINARY`, `BLOB`). Numeric and
/// temporal columns also carry it, so it marks a binary string only on a string type.
const BINARY_CHARSET: u16 = 63;

pub(crate) fn is_binary(c: &Column) -> bool {
    c.character_set() == BINARY_CHARSET
}

/// The column types that hold strings (text or binary, by the character set).
pub(crate) fn is_string(t: ColumnType) -> bool {
    use ColumnType::*;
    matches!(
        t,
        MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BLOB
            | MYSQL_TYPE_VAR_STRING
            | MYSQL_TYPE_VARCHAR
            | MYSQL_TYPE_STRING
    )
}

/// The bits of a `BIT(n)` value (big-endian bytes) as a number.
pub(crate) fn bit_number(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(0u64, |n, b| (n << 8) | *b as u64)
        .to_string()
}

/// The text of a column value that came as bytes: binary strings (`BINARY`, `VARBINARY`, `BLOB`)
/// and `GEOMETRY` are not supported and become empty, `BIT(n)` is its number, and everything else
/// is text (numbers and dates in the text protocol, `JSON` despite its binary character set).
fn bytes_text(c: &Column, bytes: &[u8]) -> String {
    let t = c.column_type();
    match t {
        ColumnType::MYSQL_TYPE_BIT => bit_number(bytes),
        ColumnType::MYSQL_TYPE_GEOMETRY => String::new(),
        _ if is_binary(c) && is_string(t) => String::new(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Formats a MySQL `DATE`/`DATETIME` value from `(year, month, day)` and
/// `(hour, minutes, seconds, micro_seconds)`.
fn format_mysql_date(
    (year, month, day): (u16, u8, u8),
    (hour, minutes, seconds, micro_seconds): (u8, u8, u8, u32),
    date_only: bool,
) -> String {
    if date_only {
        return format!("{:04}-{:02}-{:02}", year, month, day);
    }
    let mut text = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hour, minutes, seconds
    );
    if micro_seconds > 0 {
        text.push_str(&format!(".{:06}", micro_seconds));
    }
    text
}

fn format_mysql_time(
    negative: bool,
    days: u32,
    hours: u8,
    minutes: u8,
    seconds: u8,
    micro_seconds: u32,
) -> String {
    let total_hours = days as u64 * 24 + hours as u64;
    let sign = if negative { "-" } else { "" };
    let mut text = format!("{}{:02}:{:02}:{:02}", sign, total_hours, minutes, seconds);
    if micro_seconds > 0 {
        text.push_str(&format!(".{:06}", micro_seconds));
    }
    text
}

pub(crate) fn mysql_execute(db_url: &str, sql: &str, params: &IntMap<Str>) -> Int {
    // `exec` would return the rows of a result set, which INSERT/UPDATE/DELETE do not have; the
    // affected row count is reported by the connection instead.
    let result = match positional_params(params) {
        Some(params) => with_conn(db_url, sql, false, |conn| {
            conn.exec_drop(sql, params.clone()).map(|_| conn.affected_rows())
        }),
        None => with_conn(db_url, sql, false, |conn| execute_text(conn, sql)),
    };
    match result {
        Ok(n) => n as Int,
        Err(e) => {
            crate::runtime::db_write_failed("mysql_execute", e);
            0
        }
    }
}

/// Runs `sql`, which may hold several statements separated by `;` (e.g. `BEGIN; ...; COMMIT`),
/// with the text protocol, and returns the rows changed by all of them, as `sqlite_execute` and
/// `pg_execute` do. The first failing statement stops the run.
fn execute_text(conn: &mut Conn, sql: &str) -> Result<u64> {
    let mut result = conn.query_iter(sql)?;
    let mut changed = 0;
    while let Some(set) = result.iter() {
        // known from the OK packet before the rows (if any) are read
        changed += set.affected_rows();
        for row in set {
            row?;
        }
    }
    Ok(changed)
}

/// The options of `db_url`, with a connect timeout and TCP keepalive unless the URL sets them
/// (`tcp_connect_timeout_ms`, `tcp_keepalive_time_ms`).
/// No read timeout is set: a query may legitimately run for a long time, while keepalive still
/// detects a server that went away.
pub(crate) fn opts(db_url: &str) -> Result<Opts> {
    let opts = Opts::from_url(db_url)?;
    let connect_timeout = opts
        .get_tcp_connect_timeout()
        .or(Some(crate::runtime::network::CONNECT_TIMEOUT));
    let keepalive = opts.get_tcp_keepalive_time_ms().or(Some(60_000));
    Ok(OptsBuilder::from_opts(opts)
        .tcp_connect_timeout(connect_timeout)
        .tcp_keepalive_time_ms(keepalive)
        .into())
}

/// Server errors after which the connection is closed: `ER_SERVER_SHUTDOWN` (1053),
/// `ER_CONNECTION_KILLED` (1927, `KILL CONNECTION`), `ER_CLIENT_INTERACTION_TIMEOUT` (4031, idle
/// longer than `wait_timeout` on MySQL 8.0.24+), and the client codes "server has gone away"
/// (2006) and "lost connection during query" (2013) when a proxy reports them.
const CONNECTION_CLOSED_CODES: [u16; 5] = [1053, 1927, 4031, 2006, 2013];

/// Whether `e` means the connection itself is unusable, rather than the statement failing.
fn is_broken(e: &Error) -> bool {
    match e {
        Error::IoError(_)
        | Error::CodecError(_)
        | Error::DriverError(DriverError::PacketOutOfSync | DriverError::UnexpectedPacket) => true,
        Error::MySqlError(e) => CONNECTION_CLOSED_CODES.contains(&e.code),
        _ => false,
    }
}

struct MysqlDriver;

impl Driver<Conn, Error> for MysqlDriver {
    fn connect(&self, db_url: &str) -> std::result::Result<Conn, String> {
        opts(db_url)
            .and_then(Conn::new)
            .map_err(|e| e.to_string())
    }

    fn is_broken(&self, _conn: &Conn, e: &Error) -> bool {
        is_broken(e)
    }

    fn error_text(&self, e: Error) -> String {
        e.to_string()
    }

    fn rollback(&self, conn: &mut Conn) -> bool {
        conn.query_drop("ROLLBACK").is_ok()
    }
}

/// Runs `f`, which runs `sql`, with a cached connection for `db_url` that only the current thread
/// uses, so session state such as a transaction opened by `BEGIN` in an earlier call is still
/// there. No lock is held while connecting or querying. A connection that broke (server restart,
/// idle timeout) is dropped so the next call reconnects; with `retry`, `f` is also retried once on
/// a new connection: only for reads ([`is_read_only_sql`]), since a write may have been applied
/// before it was lost.
/// Neither happens silently when a transaction was open on it: see [`SessionPool`].
fn with_conn<T>(
    db_url: &str,
    sql: &str,
    retry: bool,
    f: impl FnMut(&mut Conn) -> Result<T>,
) -> std::result::Result<T, String> {
    MYSQL_CONNECTIONS.run(db_url, sql, retry, &MysqlDriver, f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike() {}

    #[test]
    fn bytes_text_by_column_type() {
        let col = |t: ColumnType, charset: u16| Column::new(t).with_character_set(charset);
        // binary strings are not supported: empty, whatever the bytes (no bare newline)
        for t in [
            ColumnType::MYSQL_TYPE_VAR_STRING,
            ColumnType::MYSQL_TYPE_STRING,
            ColumnType::MYSQL_TYPE_BLOB,
        ] {
            assert_eq!(bytes_text(&col(t, BINARY_CHARSET), b"\x0a"), "");
            assert_eq!(bytes_text(&col(t, BINARY_CHARSET), b"\xff"), "");
        }
        // text strings keep their text
        assert_eq!(
            bytes_text(&col(ColumnType::MYSQL_TYPE_VAR_STRING, 255), b"a\nb"),
            "a\nb"
        );
        // numbers, dates and JSON carry the binary character set in the text protocol
        assert_eq!(
            bytes_text(&col(ColumnType::MYSQL_TYPE_LONGLONG, BINARY_CHARSET), b"42"),
            "42"
        );
        assert_eq!(
            bytes_text(&col(ColumnType::MYSQL_TYPE_JSON, BINARY_CHARSET), b"[1]"),
            "[1]"
        );
        // BIT(n) is its number
        assert_eq!(
            bytes_text(&col(ColumnType::MYSQL_TYPE_BIT, BINARY_CHARSET), &[0x02, 0x01]),
            "513"
        );
    }

    #[test]
    fn test_is_broken() {
        let server = |code| {
            Error::MySqlError(MySqlError {
                state: "HY000".into(),
                message: String::new(),
                code,
            })
        };
        for code in CONNECTION_CLOSED_CODES {
            assert!(is_broken(&server(code)), "{code}");
        }
        // a failing statement (no such table, duplicate key, KILL QUERY) keeps the connection
        for code in [1146, 1062, 1317] {
            assert!(!is_broken(&server(code)), "{code}");
        }
    }

    #[test]
    fn test_format_mysql_date() {
        assert_eq!(
            format_mysql_date((2024, 1, 5), (3, 4, 5, 0), false),
            "2024-01-05 03:04:05"
        );
        assert_eq!(
            format_mysql_date((2024, 1, 5), (3, 4, 5, 123), false),
            "2024-01-05 03:04:05.000123"
        );
        assert_eq!(
            format_mysql_date((2024, 1, 5), (0, 0, 0, 0), true),
            "2024-01-05"
        );
    }

    #[test]
    fn test_format_mysql_time() {
        assert_eq!(format_mysql_time(false, 0, 3, 4, 5, 0), "03:04:05");
        assert_eq!(format_mysql_time(true, 1, 2, 3, 4, 0), "-26:03:04");
        assert_eq!(
            format_mysql_time(false, 34, 22, 59, 59, 500000),
            "838:59:59.500000"
        );
    }

    #[test]
    fn test_query() {
        let sql = "SELECT id, name FROM people";
        let db_url = "mysql://root:123456@127.0.0.1:3306/test";
        let rows = mysql_query(db_url, sql, &IntMap::default());
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    fn test_delete_row() {
        let sql = "delete from people where id ='11'";
        let db_url = "mysql://root:123456@127.0.0.1:3306/test";
        let _ = mysql_execute(db_url, sql, &IntMap::default());
    }
}
