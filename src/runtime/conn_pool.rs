//! Connection caches shared by the database, KV and messaging functions, keyed by URL or path.
//!
//! The global lock of a cache is only held to look up an entry: connecting and running
//! statements happen outside of it, so with `-p` a slow query or connection to one URL does
//! not block calls to other URLs.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};

/// Idle connections by URL. A caller takes a connection out (or creates one when none is idle),
/// uses it without any lock held, and gives it back, so parallel calls to the same URL run on
/// separate connections.
///
/// A pool made by [`ConnPool::new`] shares idle connections among all threads: for clients
/// without session state, such as Redis and MQTT. One made by [`ConnPool::per_thread`] only ever
/// gives a thread back its own connections: session state such as an open transaction (`BEGIN` in
/// one call, `COMMIT` in a later one), `SET` variables or temporary tables stays with the worker
/// that created it, and is never picked up by another worker running with `-p`. The connections
/// of a thread that exited are not reused: worker threads live for the whole run.
pub(crate) struct ConnPool<C> {
    idle: Mutex<HashMap<(Option<ThreadId>, String), Vec<C>>>,
    per_thread: bool,
}

impl<C> ConnPool<C> {
    /// A pool whose idle connections are shared by all threads.
    pub(crate) fn new() -> Self {
        ConnPool {
            idle: Mutex::new(HashMap::new()),
            per_thread: false,
        }
    }

    /// A pool that gives each thread back only the connections it used.
    pub(crate) fn per_thread() -> Self {
        ConnPool {
            idle: Mutex::new(HashMap::new()),
            per_thread: true,
        }
    }

    fn owner(&self, key: &str) -> (Option<ThreadId>, String) {
        let thread = self.per_thread.then(|| thread::current().id());
        (thread, key.to_string())
    }

    /// An idle connection for `key` (last used by the current thread for a per-thread pool),
    /// removed from the pool until it is given back with `put`.
    pub(crate) fn take(&self, key: &str) -> Option<C> {
        let owner = self.owner(key);
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&owner)
            .and_then(Vec::pop)
    }

    /// Give back a connection that is still usable (kept for the current thread for a
    /// per-thread pool).
    pub(crate) fn put(&self, key: &str, conn: C) {
        let owner = self.owner(key);
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(owner)
            .or_default()
            .push(conn);
    }

    /// Run `f` with every idle connection and its key.
    pub(crate) fn for_each_idle(&self, mut f: impl FnMut(&str, &C)) {
        let idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
        for ((_, key), conns) in idle.iter() {
            conns.iter().for_each(|c| f(key, c));
        }
    }
}

/// One connection per key, behind its own lock: calls to the same key are serialized, calls to
/// different keys are not. Used for SQLite, where a file is best written by a single connection
/// and every connection to `:memory:` is a different database, and for NATS, where one client is
/// shared by cloning it out of the slot.
pub(crate) struct ConnSlots<C> {
    slots: Mutex<HashMap<String, Arc<Mutex<Option<C>>>>>,
}

impl<C> ConnSlots<C> {
    pub(crate) fn new() -> Self {
        ConnSlots {
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Run `f` with the connection for `key`, opened by `open` on first use. Only the lock of
    /// `key` is held while opening and running `f`.
    pub(crate) fn with_conn<T, E>(
        &self,
        key: &str,
        open: impl FnOnce() -> Result<C, E>,
        f: impl FnOnce(&mut C) -> Result<T, E>,
    ) -> Result<T, E> {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            match slots.get(key) {
                Some(slot) => slot.clone(),
                None => {
                    let slot = Arc::new(Mutex::new(None));
                    slots.insert(key.to_string(), slot.clone());
                    slot
                }
            }
        };
        let mut guard = slot.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(open()?);
        }
        f(guard.as_mut().unwrap())
    }
}

impl<C> ConnSlots<C> {
    /// Run `f` with every open connection and its key, skipping a connection in use by another
    /// thread. Used at exit to report SQLite transactions that were never committed.
    pub(crate) fn for_each(&self, mut f: impl FnMut(&str, &mut C)) {
        let slots: Vec<(String, Arc<Mutex<Option<C>>>)> = self
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (key, slot) in slots {
            if let Ok(mut guard) = slot.try_lock()
                && let Some(conn) = guard.as_mut()
            {
                f(&key, conn);
            }
        }
    }
}

/// The SQL dialect of a database: what [`tx_controls`] takes as quotes, comments and
/// transaction control, and [`is_read_only_sql`] as a function with side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    Sqlite,
    MySql,
    Postgres,
}

/// A statement that starts or ends a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxControl {
    Begin,
    Commit,
    Rollback,
}

/// The transaction control of each statement of `sql` (statements separated by `;`, outside of
/// quotes and comments as `dialect` reads them), `None` for any other statement.
/// `ROLLBACK TO SAVEPOINT`, `COMMIT AND CHAIN` and `COMMIT PREPARED` do not end the transaction.
/// In MySQL, the statements that commit implicitly (`CREATE`, `ALTER`, `DROP`, `TRUNCATE`,
/// `LOCK TABLES`, ...) are a `Commit`, and nothing after a `CREATE PROCEDURE`/`FUNCTION`/
/// `TRIGGER`/`EVENT` (whose body has `BEGIN ... END`) is transaction control. Other implicit
/// commits are not seen: a transaction is then still taken as open, which only matters if its
/// connection is lost (the next call asks for a `ROLLBACK`) or at exit (a warning).
/// In PostgreSQL, the statements of a `CREATE FUNCTION|PROCEDURE ... BEGIN ATOMIC ... END` body,
/// and in SQLite those of a `CREATE TRIGGER ... BEGIN ... END` body, up to the `END`, are not
/// transaction control either.
pub(crate) fn tx_controls(sql: &str, dialect: Dialect) -> Vec<Option<TxControl>> {
    let mut in_routine = false;
    let mut in_body = false;
    split_statements(sql, dialect)
        .into_iter()
        .map(|stmt| {
            let words = leading_words(stmt, 6);
            if in_body {
                // the body's statements cannot start with `END` (a `CASE ... END` is inside one)
                in_body = words.first().is_none_or(|w| w != "END");
                return None;
            }
            if dialect == Dialect::MySql && !in_routine && creates_routine(&words) {
                in_routine = true;
                return Some(TxControl::Commit);
            }
            if in_routine {
                return None;
            }
            if opens_body(stmt, &words, dialect) {
                in_body = true;
                return None;
            }
            statement_control(&words, dialect)
        })
        .collect()
}

/// Whether `stmt` (with leading `words`) is a `CREATE` whose body is a `BEGIN [ATOMIC] ... END`
/// of statements separated by `;`, which goes on after `stmt`: PostgreSQL `BEGIN ATOMIC`
/// function/procedure bodies, SQLite trigger bodies.
fn opens_body(stmt: &str, words: &[String], dialect: Dialect) -> bool {
    if words.first().is_none_or(|w| w != "CREATE") {
        return false;
    }
    let all = leading_words(stmt, usize::MAX);
    let begin = match dialect {
        Dialect::Postgres => all.windows(2).position(|w| w[0] == "BEGIN" && w[1] == "ATOMIC"),
        Dialect::Sqlite if all.iter().any(|w| w == "TRIGGER") => {
            all.iter().position(|w| w == "BEGIN")
        }
        _ => None,
    };
    let body_start = match (dialect, begin) {
        (Dialect::Postgres, Some(p)) => p + 2,
        (_, Some(p)) => p + 1,
        (_, None) => return false,
    };
    // an empty body `BEGIN ATOMIC END` ends in `stmt`
    all.get(body_start).is_none_or(|w| w != "END")
}

/// The first `n` words of `stmt`, in upper case.
fn leading_words(stmt: &str, n: usize) -> Vec<String> {
    stmt.split(|c: char| c.is_whitespace() || c == '(')
        .filter(|w| !w.is_empty())
        .take(n)
        .map(|w| w.to_ascii_uppercase())
        .collect()
}

/// `CREATE [OR REPLACE] [DEFINER = ...] PROCEDURE|FUNCTION|TRIGGER|EVENT` in MySQL.
fn creates_routine(words: &[String]) -> bool {
    if words.first().is_none_or(|w| w != "CREATE") {
        return false;
    }
    let is_definer = |w: &str| w.starts_with("DEFINER") || w == "=" || w.contains('@');
    words[1..]
        .iter()
        .find(|w| !matches!(w.as_str(), "OR" | "REPLACE" | "AGGREGATE") && !is_definer(w))
        .is_some_and(|w| matches!(w.as_str(), "PROCEDURE" | "FUNCTION" | "TRIGGER" | "EVENT"))
}

fn statement_control(words: &[String], dialect: Dialect) -> Option<TxControl> {
    let first = words.first()?.as_str();
    let rest = &words[1..];
    let has = |w: &str| rest.iter().take(3).any(|x| x == w);
    let second_is = |w: &str| rest.first().is_some_and(|x| x == w);
    match first {
        // MariaDB `BEGIN NOT ATOMIC` starts a compound statement
        "BEGIN" if !second_is("NOT") => Some(TxControl::Begin),
        "START" if second_is("TRANSACTION") => Some(TxControl::Begin),
        "COMMIT" if !has("CHAIN") && !has("PREPARED") => Some(TxControl::Commit),
        // `END` is a `COMMIT` in PostgreSQL and SQLite, the end of a compound statement in MySQL
        "END" if dialect != Dialect::MySql => Some(TxControl::Commit),
        "PREPARE" if second_is("TRANSACTION") => Some(TxControl::Commit),
        "ROLLBACK" | "ABORT" if !has("TO") && !has("CHAIN") && !has("PREPARED") => {
            Some(TxControl::Rollback)
        }
        // statements that commit the transaction before they run (a temporary table does not)
        "CREATE" | "ALTER" | "DROP" | "RENAME" | "TRUNCATE" | "GRANT" | "REVOKE" | "LOCK"
        | "ANALYZE" | "OPTIMIZE" | "REPAIR" | "FLUSH"
            if dialect == Dialect::MySql && !has("TEMPORARY") =>
        {
            Some(TxControl::Commit)
        }
        _ => None,
    }
}

/// The non-empty statements of `sql`, without the comments before them. Quotes: `'...'`,
/// `"..."`, and `` `...` `` except in PostgreSQL; a backslash escapes the next character in a
/// MySQL string and in a PostgreSQL `E'...'` string, a doubled quote is two strings in a row
/// (same result). Comments: `-- ...`, `/* ... */`, and `# ...` in MySQL (in PostgreSQL `#` is
/// the XOR operator). PostgreSQL also has `$$...$$` and `$tag$...$tag$` strings.
fn split_statements(sql: &str, dialect: Dialect) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut statements = vec![];
    let mut start = 0;
    let mut i = 0;
    let skip_to = |from: usize, end: &[u8]| -> usize {
        bytes[from..]
            .windows(end.len())
            .position(|w| w == end)
            .map_or(bytes.len(), |p| from + p + end.len())
    };
    // past the closing `quote` of a string whose text starts at `from`
    let skip_quoted = |from: usize, quote: u8, backslash: bool| -> usize {
        let mut j = from;
        while j < bytes.len() {
            match bytes[j] {
                b'\\' if backslash => j += 2,
                c if c == quote => return j + 1,
                _ => j += 1,
            }
        }
        bytes.len()
    };
    let is_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'$';
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"') => {
                let backslash = match dialect {
                    Dialect::MySql => true,
                    Dialect::Postgres => {
                        q == b'\''
                            && i > 0
                            && matches!(bytes[i - 1], b'E' | b'e')
                            && (i < 2 || !is_word(bytes[i - 2]))
                    }
                    Dialect::Sqlite => false,
                };
                i = skip_quoted(i + 1, q, backslash);
            }
            b'`' if dialect != Dialect::Postgres => i = skip_quoted(i + 1, b'`', false),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_to(i + 2, b"\n"),
            b'#' if dialect == Dialect::MySql => i = skip_to(i + 1, b"\n"),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_to(i + 2, b"*/"),
            b'$' if dialect == Dialect::Postgres => {
                // dollar quoting: `$$...$$` or `$tag$...$tag$`, not a `$1` parameter
                let tag_len = bytes[i + 1..]
                    .iter()
                    .position(|&c| !(c.is_ascii_alphanumeric() || c == b'_'));
                match tag_len {
                    Some(n) if bytes[i + 1 + n] == b'$' && !bytes[i + 1].is_ascii_digit() => {
                        let tag = &bytes[i..i + n + 2];
                        i = skip_to(i + n + 2, tag);
                    }
                    _ => i += 1,
                }
            }
            b';' => {
                statements.push(&sql[start..i]);
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    statements.push(&sql[start..]);
    statements
        .into_iter()
        .map(|s| strip_leading_comments(s, dialect))
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_leading_comments(mut s: &str, dialect: Dialect) -> &str {
    loop {
        s = s.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
        let line_comment = s.strip_prefix("--").or_else(|| {
            s.strip_prefix('#')
                .filter(|_| dialect == Dialect::MySql)
        });
        if let Some(rest) = line_comment {
            s = rest.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r);
        } else {
            return s.trim_end();
        }
    }
}

/// Whether `sql` only reads, so a `*_query` may replay it on a new connection after the pooled
/// one turned out to be closed. Conservative: every statement must be a `SELECT`/`VALUES`/
/// `TABLE`/`SHOW` (or `DESCRIBE` in MySQL), or a `WITH` without a data-modifying CTE
/// (`WITH x AS (DELETE ... RETURNING *) SELECT ...`) or a write as its main statement.
///
/// A `SELECT` that calls a built-in function with a side effect (`nextval`, `pg_advisory_lock`,
/// `GET_LOCK`, ...) or whose result depends on the session (`LAST_INSERT_ID()`, `FOUND_ROWS()`)
/// is not replayed either. A user-defined function that writes cannot be detected here: the retry
/// assumes such a `SELECT` is free of side effects, so callers should run it with `*_execute`,
/// which never retries.
pub(crate) fn is_read_only_sql(sql: &str, dialect: Dialect) -> bool {
    let statements = split_statements(sql, dialect);
    !statements.is_empty()
        && statements
            .iter()
            .all(|stmt| is_read_only_statement(stmt, dialect))
}

fn is_read_only_statement(stmt: &str, dialect: Dialect) -> bool {
    let has_word = |keywords: &[&str]| {
        stmt.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| keywords.iter().any(|k| w.eq_ignore_ascii_case(k)))
    };
    let first = stmt
        .split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    if calls_side_effect_function(stmt, dialect) {
        return false;
    }
    match first.as_str() {
        // `SELECT ... INTO new_table` creates a table, `INTO @var`/`OUTFILE` writes
        "SELECT" | "VALUES" | "TABLE" | "SHOW" => !has_word(&["INTO"]),
        "DESCRIBE" | "DESC" => dialect == Dialect::MySql,
        "WITH" => !has_word(&["INSERT", "UPDATE", "DELETE", "MERGE", "INTO"]),
        _ => false,
    }
}

/// PostgreSQL built-in functions whose effect would be repeated if the statement were replayed.
/// Entries ending in `_` are prefixes (`pg_advisory_lock`, `pg_try_advisory_xact_lock_shared`, ...).
const PG_SIDE_EFFECT_FUNCTIONS: &[&str] = &[
    "nextval",
    "setval",
    "pg_advisory_",
    "pg_try_advisory_",
    "pg_notify",
    "set_config",
    "lo_",
    "dblink_exec",
    "pg_cancel_backend",
    "pg_terminate_backend",
    "pg_reload_conf",
    "pg_rotate_logfile",
    "pg_switch_wal",
    "pg_create_",
    "pg_drop_replication_slot",
    "pg_replication_slot_advance",
    "pg_logical_slot_get_",
    "pg_logical_emit_message",
    "pg_stat_reset",
    "pg_promote",
];

/// MySQL/MariaDB built-in functions that change state (locks, sequences), wait, or read the state
/// of the session, which a new connection does not have.
const MYSQL_SIDE_EFFECT_FUNCTIONS: &[&str] = &[
    "get_lock",
    "release_lock",
    "release_all_locks",
    "sleep",
    "nextval",
    "setval",
    "lastval",
    "last_insert_id",
    "found_rows",
    "row_count",
    "master_pos_wait",
    "source_pos_wait",
    "wait_for_executed_gtid_set",
    "wait_until_sql_thread_after_gtids",
];

/// Whether `sql` names a function with a side effect in `dialect` (case-insensitive, any
/// schema).
fn calls_side_effect_function(sql: &str, dialect: Dialect) -> bool {
    let functions = match dialect {
        Dialect::Postgres => PG_SIDE_EFFECT_FUNCTIONS,
        Dialect::MySql => MYSQL_SIDE_EFFECT_FUNCTIONS,
        Dialect::Sqlite => &[],
    };
    sql.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .map(|w| w.to_ascii_lowercase())
        .any(|w| {
            functions.iter().any(|f| {
                if f.ends_with('_') {
                    w.starts_with(f)
                } else {
                    w == *f
                }
            })
        })
}

/// Whether a transaction is open after `controls` ran on a connection where `in_tx` told it
/// before. `ok` tells whether the statements succeeded: after a failure the statements after
/// the failing one did not run, so a transaction taken as open stays so, unless the one
/// statement was a `COMMIT`/`ROLLBACK` (a failed `COMMIT` ends the transaction in PostgreSQL).
/// A failed batch that may have stopped after its `BEGIN` is taken as open: [`SessionPool`] then
/// rolls it back.
fn tx_open_after(in_tx: bool, controls: &[Option<TxControl>], ok: bool) -> bool {
    if ok {
        controls.iter().flatten().fold(in_tx, |_, c| *c == TxControl::Begin)
    } else if let [Some(TxControl::Commit | TxControl::Rollback)] = controls {
        false
    } else {
        in_tx || controls.contains(&Some(TxControl::Begin))
    }
}

/// The error of every call on a connection whose transaction was lost, until `ROLLBACK`/`BEGIN`.
pub(crate) const LOST_TRANSACTION: &str = "the connection was lost inside a transaction, which \
     the server rolled back: run ROLLBACK (or BEGIN) to go on";

struct Session<C> {
    conn: C,
    /// a transaction opened by `BEGIN`/`START TRANSACTION` in an earlier call is still open
    in_tx: bool,
}

/// Database connections kept per thread as by [`ConnPool`], with whether a transaction is open
/// on each, from the `BEGIN`/`COMMIT`/`ROLLBACK` statements run on it.
///
/// A closed connection is dropped and the next call reconnects. If a transaction was open on it,
/// the server rolled it back: reconnecting silently would autocommit the next writes one by one,
/// and make the final `COMMIT` a no-op that succeeds. So the call fails, as does every later call
/// of the thread for that URL, until a `ROLLBACK` (which then succeeds) or a new `BEGIN`.
///
/// A call that opens a transaction and fails (`BEGIN; insert ...; COMMIT` with a failing insert,
/// or a batch the server refused before running any of it) has the transaction it may have
/// opened rolled back, as `sqlite_execute` does: such a batch applies all of its statements or
/// none, and the connection is not left inside a transaction nobody knows about.
pub(crate) struct SessionPool<C> {
    idle: ConnPool<Session<C>>,
    lost: Mutex<HashSet<(ThreadId, String)>>,
    dialect: Dialect,
}

/// How a [`SessionPool`] runs statements on connections of type `C` with errors of type `E`.
pub(crate) trait Driver<C, E> {
    fn connect(&self, key: &str) -> Result<C, String>;
    /// whether `e` means the connection itself is unusable, rather than the statement failing
    fn is_broken(&self, conn: &C, e: &E) -> bool;
    fn error_text(&self, e: E) -> String;
    /// Run `ROLLBACK` on `conn`: whether it succeeded.
    fn rollback(&self, conn: &mut C) -> bool;
}

impl<C> SessionPool<C> {
    pub(crate) fn new(dialect: Dialect) -> Self {
        SessionPool {
            idle: ConnPool::per_thread(),
            lost: Mutex::new(HashSet::new()),
            dialect,
        }
    }

    /// The keys of the idle connections inside a transaction (a `BEGIN` without `COMMIT`),
    /// sorted, each once: at exit their transactions are rolled back by the server.
    pub(crate) fn open_transactions(&self) -> Vec<String> {
        let mut keys = vec![];
        self.idle.for_each_idle(|key, session| {
            if session.in_tx {
                keys.push(key.to_string());
            }
        });
        keys.sort();
        keys.dedup();
        keys
    }

    fn lost(&self) -> std::sync::MutexGuard<'_, HashSet<(ThreadId, String)>> {
        self.lost.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Run `f`, which runs `sql`, with a connection for `key` that only the current thread uses.
    /// With `retry`, a read that failed because a reused connection was closed is run again on a
    /// new connection, unless a transaction was open on it.
    pub(crate) fn run<T, E>(
        &self,
        key: &str,
        sql: &str,
        retry: bool,
        driver: &impl Driver<C, E>,
        mut f: impl FnMut(&mut C) -> Result<T, E>,
    ) -> Result<T, String> {
        let controls = tx_controls(sql, self.dialect);
        let owner = (thread::current().id(), key.to_string());
        if self.lost().contains(&owner) {
            match controls.first() {
                Some(Some(TxControl::Begin | TxControl::Rollback)) => {
                    self.lost().remove(&owner);
                }
                _ => return Err(LOST_TRANSACTION.to_string()),
            }
        }
        let (mut session, reused) = match self.idle.take(key) {
            Some(session) => (session, true),
            None => {
                let conn = driver.connect(key)?;
                (Session { conn, in_tx: false }, false)
            }
        };
        let e = match f(&mut session.conn) {
            Err(e) if driver.is_broken(&session.conn, &e) => e,
            result => return self.settle(key, session, &controls, result, driver),
        };
        let in_tx = session.in_tx;
        drop(session);
        let text = driver.error_text(e);
        if in_tx {
            // a `ROLLBACK` that failed this way still ended the transaction
            if controls.first() != Some(&Some(TxControl::Rollback)) {
                self.lost().insert(owner);
            }
            return Err(format!("{} ({})", text, LOST_TRANSACTION));
        }
        if !(retry && reused) {
            return Err(format!(
                "{} (not retried: the statement may already have been applied)",
                text
            ));
        }
        let conn = driver
            .connect(key)
            .map_err(|reconnect| format!("{} (reconnect failed: {})", text, reconnect))?;
        let mut session = Session { conn, in_tx: false };
        match f(&mut session.conn) {
            Err(e) if driver.is_broken(&session.conn, &e) => Err(driver.error_text(e)),
            result => self.settle(key, session, &controls, result, driver),
        }
    }

    /// Give back a connection that is still usable, with its transaction state after `result`.
    fn settle<T, E>(
        &self,
        key: &str,
        mut session: Session<C>,
        controls: &[Option<TxControl>],
        result: Result<T, E>,
        driver: &impl Driver<C, E>,
    ) -> Result<T, String> {
        let opened = !session.in_tx && controls.contains(&Some(TxControl::Begin));
        session.in_tx = tx_open_after(session.in_tx, controls, result.is_ok());
        if result.is_err() && opened && session.in_tx && driver.rollback(&mut session.conn) {
            session.in_tx = false;
        }
        self.idle.put(key, session);
        result.map_err(|e| driver.error_text(e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_pool_take_put() {
        let pool: ConnPool<i32> = ConnPool::new();
        assert_eq!(pool.take("a"), None);
        pool.put("a", 1);
        pool.put("a", 2);
        pool.put("b", 3);
        assert_eq!(pool.take("b"), Some(3));
        assert!(matches!(pool.take("a"), Some(1 | 2)));
        assert!(matches!(pool.take("a"), Some(1 | 2)));
        assert_eq!(pool.take("a"), None);
    }

    #[test]
    fn test_pool_keeps_connections_per_thread() {
        let pool: Arc<ConnPool<i32>> = Arc::new(ConnPool::per_thread());
        pool.put("a", 1);
        // another thread never gets this thread's connection, e.g. one inside a transaction
        let other = {
            let pool = pool.clone();
            thread::spawn(move || {
                let none = pool.take("a");
                pool.put("a", 2);
                (none, pool.take("a"))
            })
        };
        assert_eq!(other.join().unwrap(), (None, Some(2)));
        assert_eq!(pool.take("a"), Some(1));
        assert_eq!(pool.take("a"), None);
    }

    #[test]
    fn test_shared_pool_reuses_across_threads() {
        let pool: Arc<ConnPool<i32>> = Arc::new(ConnPool::new());
        pool.put("a", 1);
        let pool2 = pool.clone();
        let taken = thread::spawn(move || {
            let c = pool2.take("a");
            pool2.put("a", 2);
            c
        });
        assert_eq!(taken.join().unwrap(), Some(1));
        assert_eq!(pool.take("a"), Some(2));
    }

    #[test]
    fn test_tx_controls() {
        use Dialect::*;
        use TxControl::*;
        for d in [Sqlite, MySql, Postgres] {
            let tx = |sql| tx_controls(sql, d);
            assert_eq!(tx("BEGIN"), [Some(Begin)]);
            assert_eq!(tx(" start  transaction read only"), [Some(Begin)]);
            assert_eq!(tx("commit work;"), [Some(Commit)]);
            assert_eq!(tx("rollback"), [Some(Rollback)]);
            assert_eq!(tx("ROLLBACK TO SAVEPOINT s1"), [None]);
            assert_eq!(tx("rollback to s1"), [None]);
            assert_eq!(tx("COMMIT AND CHAIN"), [None]);
            assert_eq!(tx("select 'BEGIN'"), [None]);
            assert_eq!(
                tx("BEGIN; insert into t values ('a;commit'); -- x;\n COMMIT;"),
                [Some(Begin), None, Some(Commit)]
            );
            assert_eq!(tx("/* c; */ begin; insert into t values ('it''s'); commit"), [
                Some(Begin),
                None,
                Some(Commit)
            ]);
            assert_eq!(tx(" ; ;"), []);
        }
        let pg = |sql| tx_controls(sql, Postgres);
        assert_eq!(pg("END"), [Some(Commit)]);
        assert_eq!(pg("abort"), [Some(Rollback)]);
        assert_eq!(pg("COMMIT PREPARED 'x'"), [None]);
        assert_eq!(
            pg("/* c */ begin; create function f() as $$ begin; end $$; select $1"),
            [Some(Begin), None, None]
        );
        // `#` is the XOR operator, not a comment
        assert_eq!(pg("BEGIN; select 5 # 3; COMMIT"), [Some(Begin), None, Some(Commit)]);
        // a backslash escapes in E'...' only
        assert_eq!(
            pg("BEGIN; insert into t values (E'it\\'s;'); COMMIT"),
            [Some(Begin), None, Some(Commit)]
        );
        assert_eq!(
            pg("BEGIN; insert into t values ('a\\'); COMMIT"),
            [Some(Begin), None, Some(Commit)]
        );
        // DDL does not end a transaction in PostgreSQL
        assert_eq!(pg("BEGIN; create table t(x int)"), [Some(Begin), None]);

        // a `BEGIN ATOMIC` body is not transaction control, up to its `END`
        assert_eq!(
            pg("BEGIN; create function f() returns int language sql \
                BEGIN ATOMIC select case when true then 1 end; select 2; END; COMMIT"),
            [Some(Begin), None, None, None, Some(Commit)]
        );
        assert_eq!(
            pg("create procedure p() begin atomic end; end"),
            [None, Some(Commit)]
        );
        let lite = |sql| tx_controls(sql, Sqlite);
        assert_eq!(
            lite("BEGIN; create trigger t after insert on x begin delete from y; END; COMMIT"),
            [Some(Begin), None, None, Some(Commit)]
        );
        assert_eq!(lite("create table t(begin int); end"), [None, Some(Commit)]);

        let my = |sql| tx_controls(sql, MySql);
        // `\'` does not end a MySQL string
        assert_eq!(
            my("BEGIN; INSERT INTO t VALUES ('it\\'s; ok'); COMMIT"),
            [Some(Begin), None, Some(Commit)]
        );
        assert_eq!(
            my("BEGIN; # note; ROLLBACK\n insert into t values (\"a\\\"b\"); COMMIT"),
            [Some(Begin), None, Some(Commit)]
        );
        // DDL commits implicitly, a temporary table does not
        assert_eq!(my("BEGIN; create table t(x int)"), [Some(Begin), Some(Commit)]);
        assert_eq!(my("BEGIN; create temporary table t(x int)"), [Some(Begin), None]);
        assert_eq!(my("lock tables t write"), [Some(Commit)]);
        // the body of a routine is not transaction control
        assert_eq!(
            my("CREATE PROCEDURE p() BEGIN insert into t values (1); BEGIN select 1; END; END"),
            [Some(Commit), None, None, None]
        );
        assert_eq!(my("create definer=`root`@`%` procedure p() begin select 1; end"), [
            Some(Commit),
            None
        ]);
        assert_eq!(my("create table t (trigger int); begin"), [Some(Commit), Some(Begin)]);
        assert_eq!(my("END"), [None]);
        assert_eq!(my("BEGIN NOT ATOMIC select 1; END"), [None, None]);
    }

    #[test]
    fn test_read_only_sql() {
        use Dialect::*;
        for d in [MySql, Postgres] {
            let ro = |sql| is_read_only_sql(sql, d);
            assert!(ro("select * from people"));
            assert!(ro("  -- note\n /* c */ (SELECT 1) UNION (SELECT 2)"));
            assert!(ro("with t as (select 1) select * from t"));
            assert!(ro("values (1), (2)"));
            assert!(ro("SHOW tables"));
            assert!(ro("select 1; select 2"));
            assert!(!ro("insert into people(id) values (1) returning id"));
            assert!(!ro("UPDATE people SET age = 1 RETURNING *"));
            assert!(!ro("delete from people returning *"));
            assert!(!ro("with d as (delete from people returning *) select * from d"));
            assert!(!ro("with t as (select 1) insert into people select * from t"));
            assert!(!ro("select *\ninto backup from people"));
            assert!(!ro("call do_something()"));
            assert!(!ro(""));
            assert!(!ro(" ; "));
            // every statement must be a read
            assert!(!ro("select 1; delete from people"));
            assert!(!ro("insert into t values (1); select last_insert_id()"));
            assert!(!ro("select nextval('s')"));
            // similar names are still read-only
            assert!(ro("select nextval_count, lots from stats"));
        }
        let pg = |sql| is_read_only_sql(sql, Postgres);
        // built-in functions with side effects are not replayed
        assert!(!pg("SELECT setval('s', 10)"));
        assert!(!pg("select pg_advisory_lock(1)"));
        assert!(!pg("select pg_try_advisory_xact_lock(1)"));
        assert!(!pg("select pg_catalog.NEXTVAL('s')"));
        assert!(!pg("select pg_notify('ch', 'hi')"));
        assert!(!pg("select set_config('a.b', '1', false)"));
        assert!(!pg("with t as (select nextval('s')) select * from t"));
        assert!(pg("select currval('s')"));
        assert!(pg("select 5 # 3"));

        let my = |sql| is_read_only_sql(sql, MySql);
        assert!(!my("SELECT GET_LOCK('a', 10)"));
        assert!(!my("select release_lock('a')"));
        assert!(!my("select sleep(1)"));
        assert!(!my("select LAST_INSERT_ID()"));
        assert!(!my("select found_rows()"));
        assert!(!my("select id into @x from people limit 1"));
        assert!(my("describe people"));
        assert!(my("select 'a\\'; delete from t'"));
        assert!(!pg("describe people"));
    }

    #[test]
    fn test_tx_open_after() {
        use TxControl::*;
        assert!(tx_open_after(false, &[Some(Begin), None], true));
        assert!(!tx_open_after(true, &[Some(Begin), None, Some(Commit)], true));
        assert!(tx_open_after(true, &[None], true));
        // a failed batch may have stopped after its BEGIN
        assert!(tx_open_after(false, &[Some(Begin), None, Some(Commit)], false));
        // a failed COMMIT/ROLLBACK ends the transaction
        assert!(!tx_open_after(true, &[Some(Commit)], false));
        assert!(tx_open_after(true, &[None], false));
    }

    /// Connections are numbers; a statement fails with `Err(true)` when the connection broke.
    struct Fake {
        next: std::cell::Cell<u32>,
        rollbacks: std::cell::RefCell<Vec<u32>>,
    }

    impl Fake {
        fn new() -> Self {
            Fake {
                next: std::cell::Cell::new(0),
                rollbacks: std::cell::RefCell::new(vec![]),
            }
        }
    }

    impl Driver<u32, bool> for Fake {
        fn connect(&self, _key: &str) -> Result<u32, String> {
            self.next.set(self.next.get() + 1);
            Ok(self.next.get())
        }
        fn is_broken(&self, _conn: &u32, e: &bool) -> bool {
            *e
        }
        fn error_text(&self, e: bool) -> String {
            if e { "broken" } else { "failed" }.to_string()
        }
        fn rollback(&self, conn: &mut u32) -> bool {
            self.rollbacks.borrow_mut().push(*conn);
            true
        }
    }

    #[test]
    fn test_session_pool_lost_transaction() {
        let pool: SessionPool<u32> = SessionPool::new(Dialect::Postgres);
        let fake = Fake::new();
        let ok = |c: &mut u32| Ok::<u32, bool>(*c);
        let broken = |_: &mut u32| Err::<u32, bool>(true);
        // outside a transaction, a read is retried on a new connection
        assert_eq!(pool.run("db", "select 1", true, &fake, ok), Ok(1));
        assert_eq!(pool.run("db", "select 1", true, &fake, broken), Err("broken".into()));
        let mut first = true;
        let flaky = |c: &mut u32| {
            if std::mem::take(&mut first) {
                Err(true)
            } else {
                Ok(*c)
            }
        };
        assert_eq!(pool.run("db", "select 1", true, &fake, ok), Ok(3));
        assert_eq!(pool.run("db", "select 1", true, &fake, flaky), Ok(4));
        // a write is not retried
        let e = pool.run("db", "insert", false, &fake, broken).unwrap_err();
        assert!(e.contains("not retried"), "{e}");

        // inside a transaction, nothing is retried and later calls fail until ROLLBACK
        assert_eq!(pool.run("db", "BEGIN", false, &fake, ok), Ok(5));
        assert_eq!(pool.run("db", "insert", false, &fake, ok), Ok(5));
        let e = pool.run("db", "select 1", true, &fake, broken).unwrap_err();
        assert!(e.contains(LOST_TRANSACTION), "{e}");
        for sql in ["insert", "COMMIT", "select 1"] {
            assert_eq!(pool.run("db", sql, true, &fake, ok), Err(LOST_TRANSACTION.into()));
        }
        // another URL is not affected
        assert_eq!(pool.run("other", "select 1", true, &fake, ok), Ok(6));
        assert_eq!(pool.run("db", "rollback", false, &fake, ok), Ok(7));
        assert_eq!(pool.run("db", "insert", false, &fake, ok), Ok(7));

        // a BEGIN also starts over; after COMMIT a lost connection is reconnected silently
        assert_eq!(pool.run("db", "BEGIN", false, &fake, ok), Ok(7));
        assert!(pool.run("db", "insert", false, &fake, broken).is_err());
        assert_eq!(pool.run("db", "BEGIN", false, &fake, ok), Ok(8));
        assert_eq!(pool.run("db", "COMMIT", false, &fake, ok), Ok(8));
        let e = pool.run("db", "insert", false, &fake, broken).unwrap_err();
        assert!(e.contains("not retried"), "{e}");
        assert_eq!(pool.run("db", "insert", false, &fake, ok), Ok(9));
        // a statement error keeps the connection and its transaction
        assert_eq!(pool.run("db", "BEGIN", false, &fake, ok), Ok(9));
        let failed = |_: &mut u32| Err::<u32, bool>(false);
        assert_eq!(pool.run("db", "insert", false, &fake, failed), Err("failed".into()));
        let e = pool.run("db", "insert", false, &fake, broken).unwrap_err();
        assert!(e.contains(LOST_TRANSACTION), "{e}");
        assert!(fake.rollbacks.borrow().is_empty());
    }

    #[test]
    fn test_session_pool_failed_batch_rolls_back() {
        let pool: SessionPool<u32> = SessionPool::new(Dialect::Postgres);
        let fake = Fake::new();
        let ok = |c: &mut u32| Ok::<u32, bool>(*c);
        let failed = |_: &mut u32| Err::<u32, bool>(false);
        let broken = |_: &mut u32| Err::<u32, bool>(true);
        let batch = "BEGIN; insert into t values (1); COMMIT";
        // a failed batch has the transaction it opened rolled back: no transaction is left
        assert_eq!(pool.run("db", batch, false, &fake, failed), Err("failed".into()));
        assert_eq!(*fake.rollbacks.borrow(), [1]);
        assert!(pool.open_transactions().is_empty());
        let e = pool.run("db", "insert", false, &fake, broken).unwrap_err();
        assert!(e.contains("not retried"), "{e}");
        // a transaction opened by an earlier call is left to the program
        assert_eq!(pool.run("db", "BEGIN", false, &fake, ok), Ok(2));
        assert_eq!(pool.run("db", batch, false, &fake, failed), Err("failed".into()));
        assert_eq!(*fake.rollbacks.borrow(), [1]);
        assert_eq!(pool.open_transactions(), ["db"]);
        assert_eq!(pool.run("other", "BEGIN; insert", false, &fake, ok), Ok(3));
        assert_eq!(pool.open_transactions(), ["db", "other"]);
        assert_eq!(pool.run("db", "rollback", false, &fake, ok), Ok(2));
        assert_eq!(pool.open_transactions(), ["other"]);
    }

    #[test]
    fn test_slots_do_not_block_other_keys() {
        let slots: Arc<ConnSlots<i32>> = Arc::new(ConnSlots::new());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let busy = {
            let slots = slots.clone();
            thread::spawn(move || {
                slots.with_conn(
                    "slow",
                    || Ok::<_, ()>(1),
                    |_| {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(())
                    },
                )
            })
        };
        started_rx.recv().unwrap();
        // "slow" is still in use: another key opens and runs without waiting for it
        let (done_tx, done_rx) = mpsc::channel();
        let other = {
            let slots = slots.clone();
            thread::spawn(move || {
                let v = slots.with_conn("fast", || Ok::<_, ()>(2), |c| Ok(*c));
                done_tx.send(v).unwrap();
            })
        };
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(2));
        release_tx.send(()).unwrap();
        busy.join().unwrap().unwrap();
        other.join().unwrap();
        // the connection is opened once and reused
        assert_eq!(slots.with_conn("slow", || Err(()), |c| Ok(*c)), Ok(1));
    }
}
