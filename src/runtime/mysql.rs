use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::{f32_to_text, float_to_text};
use crate::runtime::str_escape::{is_null, null_input};
use crate::runtime::{Int, IntMap, Str, stdlib_warning};
use mysql::consts::ColumnType;
use mysql::prelude::*;
use mysql::*;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

static MYSQL_POOLS: LazyLock<Arc<Mutex<HashMap<String, Pool>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

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
    let result = match positional_params(params) {
        Some(params) => with_conn(db_url, |conn| conn.exec(sql, params)),
        None => with_conn(db_url, |conn| conn.query(sql)),
    };
    let rows: Vec<Row> = match result {
        Ok(rows) => rows,
        Err(e) => {
            stdlib_warning("mysql_query", e);
            return map;
        }
    };
    let mut index = 1;
    for row in rows {
        let mut items: Vec<String> = vec![];
        for i in 0..row.len() {
            let col_value: Value = row.get(i).unwrap_or(Value::NULL);
            let text_value = match col_value {
                Value::NULL => null_input().to_owned(),
                Value::Bytes(bytes) => String::from_utf8(bytes).unwrap_or("".to_owned()),
                Value::Int(num) => num.to_string(),
                Value::UInt(num) => num.to_string(),
                Value::Float(num) => f32_to_text(num),
                Value::Double(num) => float_to_text(num),
                Value::Date(year, month, day, hour, minutes, seconds, micro_seconds) => {
                    let date_only =
                        row.columns_ref()[i].column_type() == ColumnType::MYSQL_TYPE_DATE;
                    format_mysql_date(
                        year,
                        month,
                        day,
                        hour,
                        minutes,
                        seconds,
                        micro_seconds,
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
        index += 1;
    }
    map
}

fn format_mysql_date(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minutes: u8,
    seconds: u8,
    micro_seconds: u32,
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
        Some(params) => with_conn(db_url, |conn| {
            conn.exec_drop(sql, params).map(|_| conn.affected_rows())
        }),
        None => with_conn(db_url, |conn| {
            conn.query_drop(sql).map(|_| conn.affected_rows())
        }),
    };
    match result {
        Ok(n) => n as Int,
        Err(e) => {
            stdlib_warning("mysql_execute", e);
            0
        }
    }
}

/// Pool bounds used unless the URL sets `pool_min`/`pool_max`: the crate default (min 10, max 100)
/// eagerly opens 10 sessions per URL, which exhausts `max_connections` when a script talks to a few
/// DSNs. Connections are opened on demand instead, at most a handful per URL.
const POOL_MIN: usize = 0;
const POOL_MAX: usize = 8;

/// How long to wait for a free pooled connection before failing, instead of blocking forever.
const POOL_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether the query string of `db_url` sets the parameter `name`.
fn has_url_param(db_url: &str, name: &str) -> bool {
    db_url.split_once('?').is_some_and(|(_, query)| {
        query
            .split('&')
            .any(|pair| pair.split('=').next() == Some(name))
    })
}

/// The options of `db_url`, with a connect timeout, TCP keepalive and small lazy pool bounds unless
/// the URL sets them (`tcp_connect_timeout_ms`, `tcp_keepalive_time_ms`, `pool_min`, `pool_max`).
/// No read timeout is set: a query may legitimately run for a long time, while keepalive still
/// detects a server that went away.
pub(crate) fn opts(db_url: &str) -> Result<Opts> {
    let opts = Opts::from_url(db_url)?;
    let connect_timeout = opts
        .get_tcp_connect_timeout()
        .or(Some(crate::runtime::network::CONNECT_TIMEOUT));
    let keepalive = opts.get_tcp_keepalive_time_ms().or(Some(60_000));
    let pool_opts = if has_url_param(db_url, "pool_min") || has_url_param(db_url, "pool_max") {
        opts.get_pool_opts().clone()
    } else {
        let constraints = PoolConstraints::new(POOL_MIN, POOL_MAX).expect("valid pool bounds");
        opts.get_pool_opts().clone().with_constraints(constraints)
    };
    Ok(OptsBuilder::from_opts(opts)
        .tcp_connect_timeout(connect_timeout)
        .tcp_keepalive_time_ms(keepalive)
        .pool_opts(pool_opts)
        .into())
}

/// Runs `f` with a connection from the cached pool for `db_url`, creating the pool on first use.
/// The global lock is only held to clone the pool: connecting and querying run outside of it.
fn with_conn<T>(db_url: &str, f: impl FnOnce(&mut PooledConn) -> Result<T>) -> Result<T> {
    let cached = MYSQL_POOLS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(db_url)
        .cloned();
    let pool = match cached {
        Some(pool) => pool,
        None => {
            let pool = Pool::new(opts(db_url)?)?;
            // another thread may have created one meanwhile: keep the first
            MYSQL_POOLS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry(db_url.to_string())
                .or_insert(pool)
                .clone()
        }
    };
    let mut conn = pool.try_get_conn(POOL_ACQUIRE_TIMEOUT)?;
    f(&mut conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike() {}

    #[test]
    fn test_pool_constraints() {
        let bounds = |url: &str| {
            let c = opts(url).unwrap().get_pool_opts().constraints();
            (c.min(), c.max())
        };
        assert_eq!(
            bounds("mysql://root@127.0.0.1:3306/test"),
            (POOL_MIN, POOL_MAX)
        );
        assert_eq!(
            bounds("mysql://root@127.0.0.1:3306/test?pool_min=2&pool_max=3"),
            (2, 3)
        );
        assert_eq!(
            bounds("mysql://root@127.0.0.1:3306/test?pool_max=20"),
            (10, 20)
        );
    }

    #[test]
    fn test_format_mysql_date() {
        assert_eq!(
            format_mysql_date(2024, 1, 5, 3, 4, 5, 0, false),
            "2024-01-05 03:04:05"
        );
        assert_eq!(
            format_mysql_date(2024, 1, 5, 3, 4, 5, 123, false),
            "2024-01-05 03:04:05.000123"
        );
        assert_eq!(
            format_mysql_date(2024, 1, 5, 0, 0, 0, 0, true),
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
