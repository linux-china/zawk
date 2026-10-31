//! Database tables and queries as input: `sqlite://path#table`, `mysql://...#table` and
//! `postgresql://...#table` (or `postgres://`).
//!
//! The text after `#` is a table name, or a SQL query when it contains whitespace
//! (`sqlite://app.db#select nick, age from user where age > 30`). The rows are converted to CSV
//! with a header line of the column names, which is then read like `-i csv -H` input: `$1..$NF`
//! are the columns, and `FI` maps each column name to its index.
//!
//! Values are plain text: NULL is empty (or the `--null` text), numbers and dates as the database
//! shows them, and binary values (BLOB, BYTEA, binary MySQL strings) are empty.
//!
//! The query runs on a thread of its own, which sends the rows over a bounded channel as they are
//! read, so a large table is streamed rather than loaded into memory.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

/// A value of a column.
#[derive(Clone, Debug, PartialEq)]
pub enum DbValue {
    Null,
    /// A number, as its text.
    Num(String),
    Text(String),
}

impl DbValue {
    /// The text of the value as awk sees it.
    pub fn text(&self) -> &str {
        match self {
            DbValue::Null => crate::runtime::str_escape::null_input(),
            DbValue::Num(s) | DbValue::Text(s) => s,
        }
    }
}

/// A column of the result: its name and its type in the database (`None` when the database does
/// not tell, as for an expression in SQLite).
#[derive(Clone, Debug)]
pub struct DbColumn {
    pub name: String,
    pub db_type: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Sqlite,
    Mysql,
    Postgres,
}

/// The kind of database of `url`, from its scheme.
fn kind(url: &str) -> Option<Kind> {
    let scheme = url.split_once("://")?.0.to_ascii_lowercase();
    match scheme.as_str() {
        "sqlite" | "sqlite3" => Some(Kind::Sqlite),
        "mysql" => Some(Kind::Mysql),
        "postgresql" | "postgres" => Some(Kind::Postgres),
        _ => None,
    }
}

/// Whether the input operand `name` is a database URL (`sqlite://`, `mysql://`, `postgresql://`).
pub fn is_db_url(name: &str) -> bool {
    kind(name).is_some()
}

/// An error reading `url`, reported as an input that cannot be opened.
fn read_error(url: &str, e: impl std::fmt::Display) -> io::Error {
    let msg = format!(
        "cannot read `{}': {}",
        crate::runtime::network::redact_url(url),
        e
    );
    io::Error::other(crate::common::InputOpenError(msg))
}

/// What a database URL reads: the connection (a SQLite path, a MySQL or PostgreSQL URL) and the
/// query.
#[derive(Debug, PartialEq)]
struct Source {
    kind: Kind,
    conn: String,
    sql: String,
}

impl Source {
    fn parse(url: &str) -> Result<Source, String> {
        let kind = kind(url).ok_or_else(|| "not a database URL".to_string())?;
        // the `#` after the userinfo: a raw password may have one (`mysql://u:p#w@h/db#t`)
        let userinfo = crate::runtime::network::userinfo_range(url);
        let after = userinfo.map_or(0, |(_, at)| at + 1);
        let (conn, target) = match url[after..].find('#').map(|i| url.split_at(after + i)) {
            Some((conn, target)) if !target[1..].trim().is_empty() => (conn, target[1..].trim()),
            _ => {
                return Err("expected `#table' or `#select ...' after the database URL".to_string());
            }
        };
        let conn = match (kind, userinfo) {
            // `sqlite://./data.db`, `sqlite:///abs/data.db`
            (Kind::Sqlite, _) => conn.split_once("://").unwrap().1.to_string(),
            (_, Some((start, at))) => encode_password(conn, start, at),
            _ => conn.to_string(),
        };
        let sql = if target.contains(char::is_whitespace) {
            target.to_string()
        } else {
            format!("SELECT * FROM {}", quote_table(kind, target))
        };
        Ok(Source { kind, conn, sql })
    }
}

/// `conn` with `/`, `?`, `#` and `@` percent-encoded in the password of its userinfo
/// `conn[start..at]`, which the MySQL and PostgreSQL drivers decode: a raw password works.
fn encode_password(conn: &str, start: usize, at: usize) -> String {
    let Some(colon) = conn[start..at].find(':').map(|i| start + i) else {
        return conn.to_string();
    };
    let mut out = conn[..=colon].to_string();
    for c in conn[colon + 1..at].chars() {
        match c {
            '/' => out.push_str("%2F"),
            '?' => out.push_str("%3F"),
            '#' => out.push_str("%23"),
            '@' => out.push_str("%40"),
            _ => out.push(c),
        }
    }
    out.push_str(&conn[at..]);
    out
}

/// The table `name` (or `schema.table`) quoted, so that reserved words (`user` in PostgreSQL)
/// are table names. Each part means what it means unquoted: PostgreSQL folds it to lower case.
fn quote_table(kind: Kind, name: &str) -> String {
    let quote = |part: &str| match kind {
        Kind::Mysql => format!("`{}`", part.replace('`', "``")),
        Kind::Sqlite => format!("\"{}\"", part.replace('"', "\"\"")),
        Kind::Postgres => format!("\"{}\"", part.to_lowercase().replace('"', "\"\"")),
    };
    name.split('.').map(quote).collect::<Vec<_>>().join(".")
}

enum Msg {
    Columns(Vec<DbColumn>),
    Rows(Vec<Vec<DbValue>>),
    Error(String),
}

/// Rows are sent in batches of this many.
const BATCH_ROWS: usize = 256;

/// Sends the result of a query to the reader, in batches.
struct Sink {
    tx: SyncSender<Msg>,
    batch: Vec<Vec<DbValue>>,
}

impl Sink {
    fn columns(&self, columns: Vec<DbColumn>) -> bool {
        self.tx.send(Msg::Columns(columns)).is_ok()
    }
    /// Adds a row; false when the reader is gone (the program stopped reading), to stop the query.
    fn row(&mut self, row: Vec<DbValue>) -> bool {
        self.batch.push(row);
        self.batch.len() < BATCH_ROWS || self.flush()
    }
    fn flush(&mut self) -> bool {
        self.batch.is_empty()
            || self
                .tx
                .send(Msg::Rows(std::mem::take(&mut self.batch)))
                .is_ok()
    }
}

/// The rows of a database table or query, read as they arrive.
pub struct DbTable {
    columns: Vec<DbColumn>,
    rx: Receiver<Msg>,
    pending: VecDeque<Vec<DbValue>>,
    url: String,
    done: bool,
}

impl DbTable {
    /// Connects and starts the query of `url`; fails if it cannot connect or the query fails
    /// before its first row.
    pub fn open(url: &str) -> io::Result<DbTable> {
        let error = |e: String| read_error(url, e);
        let source = Source::parse(url).map_err(error)?;
        let (tx, rx) = sync_channel(4);
        std::thread::Builder::new()
            .name("zawk-db-input".to_string())
            .spawn(move || {
                let mut sink = Sink {
                    tx: tx.clone(),
                    batch: Vec::new(),
                };
                let result = match source.kind {
                    Kind::Sqlite => sqlite::query(&source, &mut sink),
                    Kind::Mysql => mysql::query(&source, &mut sink),
                    Kind::Postgres => postgres::query(&source, &mut sink),
                };
                match result {
                    Ok(()) => {
                        sink.flush();
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e));
                    }
                }
            })?;
        let mut table = DbTable {
            columns: Vec::new(),
            rx,
            pending: VecDeque::new(),
            url: url.to_string(),
            done: false,
        };
        match table.rx.recv() {
            Ok(Msg::Columns(columns)) => table.columns = columns,
            Ok(Msg::Error(e)) => return Err(error(e)),
            _ => return Err(error("the query returned no result".to_string())),
        }
        Ok(table)
    }

    pub fn columns(&self) -> &[DbColumn] {
        &self.columns
    }

    pub fn next_row(&mut self) -> Option<io::Result<Vec<DbValue>>> {
        loop {
            if let Some(row) = self.pending.pop_front() {
                return Some(Ok(row));
            }
            if self.done {
                return None;
            }
            match self.rx.recv() {
                Ok(Msg::Rows(rows)) => self.pending.extend(rows),
                Ok(Msg::Error(e)) => {
                    self.done = true;
                    return Some(Err(read_error(&self.url, e)));
                }
                // the query is done
                Ok(Msg::Columns(_)) | Err(_) => self.done = true,
            }
        }
    }
}

/// A database table or query as CSV text, with a header line of the column names. It connects
/// on the first read.
pub struct DbCsv {
    url: String,
    table: Option<DbTable>,
    buf: Vec<u8>,
    pos: usize,
}

impl DbCsv {
    pub fn new(url: String) -> DbCsv {
        DbCsv {
            url,
            table: None,
            buf: Vec::new(),
            pos: 0,
        }
    }

    fn push_record<'a>(&mut self, fields: impl Iterator<Item = &'a str>) {
        let fields: Vec<&str> = fields.collect();
        self.buf
            .extend_from_slice(crate::runtime::csv::vec_to_csv(&fields).as_bytes());
        self.buf.push(b'\n');
    }

    /// Fills `buf` with the next rows; false at the end of the result.
    fn fill(&mut self) -> io::Result<bool> {
        self.buf.clear();
        self.pos = 0;
        if self.table.is_none() {
            let table = DbTable::open(&self.url)?;
            let names: Vec<String> = table.columns().iter().map(|c| c.name.clone()).collect();
            self.push_record(names.iter().map(String::as_str));
            self.table = Some(table);
            return Ok(true);
        }
        let table = self.table.as_mut().unwrap();
        let mut rows = Vec::new();
        while rows.len() < BATCH_ROWS {
            match table.next_row() {
                Some(row) => rows.push(row?),
                None => break,
            }
        }
        if rows.is_empty() {
            return Ok(false);
        }
        for row in &rows {
            self.push_record(row.iter().map(DbValue::text));
        }
        Ok(true)
    }
}

impl Read for DbCsv {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() && !self.fill()? {
            return Ok(0);
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

mod sqlite {
    use super::{DbColumn, DbValue, Sink, Source};
    use crate::runtime::numfmt::float_to_text;
    use rusqlite::types::ValueRef;
    use rusqlite::{Connection, OpenFlags};

    pub(super) fn query(source: &Source, sink: &mut Sink) -> Result<(), String> {
        // Read only: a missing file is an error, rather than a new empty database.
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&source.conn, flags).map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare(&source.sql).map_err(|e| e.to_string())?;
        let columns: Vec<DbColumn> = stmt
            .columns()
            .map(|c| DbColumn {
                name: c.name().to_string(),
                db_type: c.decl_type().map(str::to_uppercase),
            })
            .collect();
        let n = columns.len();
        if !sink.columns(columns) {
            return Ok(());
        }
        let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let mut values = Vec::with_capacity(n);
            for i in 0..n {
                values.push(match row.get_ref(i).map_err(|e| e.to_string())? {
                    ValueRef::Null => DbValue::Null,
                    ValueRef::Integer(v) => DbValue::Num(v.to_string()),
                    ValueRef::Real(v) => DbValue::Num(float_to_text(v)),
                    ValueRef::Text(v) => DbValue::Text(String::from_utf8_lossy(v).into_owned()),
                    ValueRef::Blob(_) => DbValue::Text(String::new()),
                });
            }
            if !sink.row(values) {
                return Ok(());
            }
        }
        Ok(())
    }
}

mod mysql {
    use super::{DbColumn, DbValue, Sink, Source};
    use mysql::consts::ColumnType::{self, *};
    use mysql::prelude::Queryable;
    use mysql::{Column, Conn, Value};

    /// The character set number of binary strings (`BINARY`, `VARBINARY`, `BLOB`).
    const BINARY_CHARSET: u16 = 63;

    /// The text of `e`, without the `MySqlError { ... }` around a server error.
    fn error_text(e: mysql::Error) -> String {
        match e {
            mysql::Error::MySqlError(e) => e.to_string(),
            e => e.to_string(),
        }
    }

    pub(super) fn query(source: &Source, sink: &mut Sink) -> Result<(), String> {
        let opts = crate::runtime::mysql::opts(&source.conn).map_err(error_text)?;
        let mut conn = Conn::new(opts).map_err(error_text)?;
        let mut result = conn.query_iter(&source.sql).map_err(error_text)?;
        let columns: Vec<Column> = result.columns().as_ref().to_vec();
        let db_columns = columns
            .iter()
            .map(|c| DbColumn {
                name: c.name_str().into_owned(),
                db_type: Some(type_name(c).to_string()),
            })
            .collect();
        if !sink.columns(db_columns) {
            abandon(result);
            return Ok(());
        }
        // Only the first result set is read.
        let mut stopped = false;
        if let Some(mut set) = result.iter() {
            while let Some(row) = set.next() {
                let row = row.map_err(error_text)?;
                let values = columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| value(c, row.as_ref(i)))
                    .collect();
                if !sink.row(values) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                std::mem::forget(set);
            }
        }
        if stopped {
            abandon(result);
        }
        Ok(())
    }

    /// Gives up on the rest of `result` when the reader stops early. Dropping a `QueryResult`
    /// (or a `ResultSet`) reads every remaining row first, so it is forgotten instead: dropping
    /// the connection then closes the socket, and the server aborts the query. This leaks the
    /// small state of the result (column definitions), once per abandoned query.
    fn abandon<T: mysql::prelude::Protocol>(result: mysql::QueryResult<'_, '_, '_, T>) {
        std::mem::forget(result);
    }

    fn is_binary(c: &Column) -> bool {
        c.character_set() == BINARY_CHARSET
    }

    fn is_number(t: ColumnType) -> bool {
        matches!(
            t,
            MYSQL_TYPE_TINY
                | MYSQL_TYPE_SHORT
                | MYSQL_TYPE_INT24
                | MYSQL_TYPE_LONG
                | MYSQL_TYPE_LONGLONG
                | MYSQL_TYPE_FLOAT
                | MYSQL_TYPE_DOUBLE
                | MYSQL_TYPE_DECIMAL
                | MYSQL_TYPE_NEWDECIMAL
                | MYSQL_TYPE_YEAR
        )
    }

    /// The value of a column, read with the text protocol (every value is text).
    fn value(c: &Column, v: Option<&Value>) -> DbValue {
        let t = c.column_type();
        let bytes = match v {
            None | Some(Value::NULL) => return DbValue::Null,
            Some(Value::Bytes(bytes)) => bytes,
            Some(other) => return DbValue::Text(other.as_sql(true).trim_matches('\'').to_string()),
        };
        match t {
            // BIT(n): the bits as a number
            MYSQL_TYPE_BIT => DbValue::Num(
                bytes
                    .iter()
                    .fold(0u64, |n, b| (n << 8) | *b as u64)
                    .to_string(),
            ),
            MYSQL_TYPE_GEOMETRY => DbValue::Text(String::new()),
            _ if is_number(t) => DbValue::Num(String::from_utf8_lossy(bytes).into_owned()),
            // a JSON column has the binary character set, but is text
            MYSQL_TYPE_JSON => DbValue::Text(String::from_utf8_lossy(bytes).into_owned()),
            _ if is_binary(c) && is_string(t) => DbValue::Text(String::new()),
            _ => DbValue::Text(String::from_utf8_lossy(bytes).into_owned()),
        }
    }

    fn is_string(t: ColumnType) -> bool {
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

    fn type_name(c: &Column) -> &'static str {
        let binary = is_binary(c);
        match c.column_type() {
            MYSQL_TYPE_TINY => "TINYINT",
            MYSQL_TYPE_SHORT => "SMALLINT",
            MYSQL_TYPE_INT24 => "MEDIUMINT",
            MYSQL_TYPE_LONG => "INT",
            MYSQL_TYPE_LONGLONG => "BIGINT",
            MYSQL_TYPE_FLOAT => "FLOAT",
            MYSQL_TYPE_DOUBLE => "DOUBLE",
            MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => "DECIMAL",
            MYSQL_TYPE_YEAR => "YEAR",
            MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE",
            MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME",
            MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME",
            MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP",
            MYSQL_TYPE_BIT => "BIT",
            MYSQL_TYPE_JSON => "JSON",
            MYSQL_TYPE_ENUM => "ENUM",
            MYSQL_TYPE_SET => "SET",
            MYSQL_TYPE_GEOMETRY => "GEOMETRY",
            MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR if binary => "VARBINARY",
            MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => "VARCHAR",
            MYSQL_TYPE_STRING if binary => "BINARY",
            MYSQL_TYPE_STRING => "CHAR",
            MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BLOB
                if binary =>
            {
                "BLOB"
            }
            MYSQL_TYPE_TINY_BLOB
            | MYSQL_TYPE_MEDIUM_BLOB
            | MYSQL_TYPE_LONG_BLOB
            | MYSQL_TYPE_BLOB => "TEXT",
            _ => "UNKNOWN",
        }
    }
}

mod postgres {
    use super::{DbColumn, DbValue, Sink, Source};
    use postgres::fallible_iterator::FallibleIterator;
    use postgres::types::Type;

    pub(super) fn query(source: &Source, sink: &mut Sink) -> Result<(), String> {
        let mut client = crate::runtime::postgres::connect(&source.conn)?;
        let stmt = client
            .prepare(&source.sql)
            .map_err(crate::runtime::postgres::error_text)?;
        let columns = stmt
            .columns()
            .iter()
            .map(|c| DbColumn {
                name: c.name().to_string(),
                db_type: Some(c.type_().name().to_string()),
            })
            .collect();
        if !sink.columns(columns) {
            return Ok(());
        }
        let mut rows = client
            .query_raw(&stmt, std::iter::empty::<&str>())
            .map_err(crate::runtime::postgres::error_text)?;
        while let Some(row) = rows.next().map_err(crate::runtime::postgres::error_text)? {
            let values = (0..row.len())
                .map(|i| {
                    let ty = row.columns()[i].type_();
                    if crate::runtime::postgres::is_null_column(&row, i) {
                        DbValue::Null
                    } else if *ty == Type::BYTEA {
                        DbValue::Text(String::new())
                    } else if is_number(ty) {
                        DbValue::Num(crate::runtime::postgres::reflective_get(&row, i))
                    } else {
                        DbValue::Text(crate::runtime::postgres::reflective_get(&row, i))
                    }
                })
                .collect();
            if !sink.row(values) {
                return Ok(());
            }
        }
        Ok(())
    }

    fn is_number(ty: &Type) -> bool {
        [
            Type::INT2,
            Type::INT4,
            Type::INT8,
            Type::FLOAT4,
            Type::FLOAT8,
            Type::NUMERIC,
            Type::OID,
        ]
        .contains(ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source() {
        let s = Source::parse("sqlite://./tests/demo.sqlite3#user").unwrap();
        assert_eq!(s.kind, Kind::Sqlite);
        assert_eq!(s.conn, "./tests/demo.sqlite3");
        assert_eq!(s.sql, r#"SELECT * FROM "user""#);
        let s = Source::parse("sqlite:///tmp/a.db#select nick from user").unwrap();
        assert_eq!(s.conn, "/tmp/a.db");
        assert_eq!(s.sql, "select nick from user");
        let s = Source::parse("mysql://root:123456@127.0.0.1:3306/test#people").unwrap();
        assert_eq!(s.conn, "mysql://root:123456@127.0.0.1:3306/test");
        assert_eq!(s.sql, "SELECT * FROM `people`");
        // a raw password with reserved characters
        let s = Source::parse("mysql://u:p#w/x?y@z@h:3306/db#t").unwrap();
        assert_eq!(s.conn, "mysql://u:p%23w%2Fx%3Fy%40z@h:3306/db");
        assert_eq!(s.sql, "SELECT * FROM `t`");
        let s = Source::parse("postgres://u:p%2Fw@h/db#select 'a@b#c'").unwrap();
        assert_eq!(s.conn, "postgres://u:p%2Fw@h/db");
        assert_eq!(s.sql, "select 'a@b#c'");
        let s = Source::parse("sqlite://./a.db#select 'a:b@c'").unwrap();
        assert_eq!(s.conn, "./a.db");
        assert_eq!(s.sql, "select 'a:b@c'");
        let s = Source::parse("postgresql://u:p@h/db#Public.People").unwrap();
        assert_eq!(s.kind, Kind::Postgres);
        assert_eq!(s.sql, r#"SELECT * FROM "public"."people""#);
        assert!(Source::parse("sqlite://a.db").is_err());
        assert!(Source::parse("sqlite://a.db#  ").is_err());
        assert!(!is_db_url("data.csv"));
        assert!(!is_db_url("https://example.com/a.csv"));
        assert!(is_db_url("postgres://h/db#t"));
    }

    #[test]
    fn test_sqlite_csv() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE user (nick VARCHAR, age INT, score REAL, avatar BLOB);
             INSERT INTO user VALUES ('jackie', 30, 1.5, x'0102'), ('a,b', NULL, 2, NULL);",
        )
        .unwrap();
        let url = format!("sqlite://{}#user", path.display());
        let mut text = String::new();
        DbCsv::new(url.clone()).read_to_string(&mut text).unwrap();
        assert_eq!(text, "nick,age,score,avatar\njackie,30,1.5,\n\"a,b\",,2,\n");

        let mut table = DbTable::open(&url).unwrap();
        let types: Vec<_> = table.columns().iter().map(|c| c.db_type.clone()).collect();
        assert_eq!(
            types,
            [
                Some("VARCHAR".into()),
                Some("INT".into()),
                Some("REAL".into()),
                Some("BLOB".into())
            ]
        );
        let row = table.next_row().unwrap().unwrap();
        assert_eq!(
            row,
            [
                DbValue::Text("jackie".into()),
                DbValue::Num("30".into()),
                DbValue::Num("1.5".into()),
                DbValue::Text("".into())
            ]
        );
        assert_eq!(table.next_row().unwrap().unwrap()[1], DbValue::Null);
        assert!(table.next_row().is_none());

        let url = format!("sqlite://{}#select count(*) as n from user", path.display());
        let mut text = String::new();
        DbCsv::new(url).read_to_string(&mut text).unwrap();
        assert_eq!(text, "n\n2\n");

        let err = DbTable::open(&format!("sqlite://{}#missing", path.display()))
            .err()
            .unwrap();
        assert!(err.to_string().contains("no such table"), "{}", err);
        assert!(DbTable::open("sqlite:///no/such/dir/x.db#t").is_err());
    }

    fn csv_of(url: &str) -> String {
        let mut text = String::new();
        DbCsv::new(url.to_string())
            .read_to_string(&mut text)
            .unwrap();
        text
    }

    #[test]
    #[ignore = "needs the MySQL server of AGENTS.md"]
    fn test_mysql_csv() {
        let url = "mysql://root:123456@127.0.0.1:3306/test";
        assert!(csv_of(&format!("{}#people", url)).starts_with("id,name,age\n1,Jackie,48\n"));
        let sql = "select cast('ab' as binary) as b, cast(null as char) as n, 1.50 as d, date('2024-01-05') as dt, 'x,y' as s";
        assert_eq!(
            csv_of(&format!("{}#{}", url, sql)),
            "b,n,d,dt,s\n,,1.50,2024-01-05,\"x,y\"\n"
        );
        let table = DbTable::open(&format!("{}#{}", url, sql)).unwrap();
        let types: Vec<_> = table
            .columns()
            .iter()
            .map(|c| c.db_type.clone().unwrap())
            .collect();
        assert_eq!(
            types,
            ["VARBINARY", "VARCHAR", "DECIMAL", "DATE", "VARCHAR"]
        );
        let err = DbTable::open("mysql://root:wrong@127.0.0.1:3306/test#people")
            .err()
            .unwrap()
            .to_string();
        assert!(
            err.starts_with("cannot read `mysql://root:***@") && err.contains("ERROR 1045"),
            "{}",
            err
        );
    }

    #[test]
    #[ignore = "needs the MySQL server of AGENTS.md"]
    fn test_mysql_stop_early() {
        use ::mysql::prelude::Queryable;
        let url = "mysql://root:123456@127.0.0.1:3306/test";
        // millions of rows, tagged to be found in the process list
        let sql = "select 'zawk_stop_early', a.ordinal_position, repeat('x', 200) \
                   from information_schema.columns a, information_schema.columns b, information_schema.columns c";
        let mut table = DbTable::open(&format!("{}#{}", url, sql)).unwrap();
        for _ in 0..3 {
            table.next_row().unwrap().unwrap();
        }
        drop(table);
        let opts = crate::runtime::mysql::opts(url).unwrap();
        let mut conn = ::mysql::Conn::new(opts).unwrap();
        let mut running = || {
            let rows: Vec<Option<String>> = conn
                .query("select info from information_schema.processlist where info like '%zawk_stop_early%' and info not like '%processlist%'")
                .unwrap();
            !rows.is_empty()
        };
        let start = std::time::Instant::now();
        while running() {
            assert!(
                start.elapsed() < std::time::Duration::from_secs(3),
                "the query still runs"
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    #[test]
    #[ignore = "needs the PostgreSQL server of AGENTS.md"]
    fn test_postgres_csv() {
        let url = "postgresql://postgres:123456@127.0.0.1:15432/demo_db";
        assert!(csv_of(&format!("{}#people", url)).starts_with("id,name,age\n1,Jackie,48\n"));
        let sql = "select '\\x0102'::bytea as b, null::int as n, 1.50::numeric as d, date '2024-01-05' as dt, 'x,y' as s";
        assert_eq!(
            csv_of(&format!("{}#{}", url, sql)),
            "b,n,d,dt,s\n,,1.50,2024-01-05,\"x,y\"\n"
        );
        let mut table = DbTable::open(&format!("{}#{}", url, sql)).unwrap();
        let types: Vec<_> = table
            .columns()
            .iter()
            .map(|c| c.db_type.clone().unwrap())
            .collect();
        assert_eq!(types, ["bytea", "int4", "numeric", "date", "text"]);
        let row = table.next_row().unwrap().unwrap();
        assert_eq!(row[1], DbValue::Null);
        assert_eq!(row[2], DbValue::Num("1.50".into()));
    }
}
