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

/// A statement that starts or ends a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TxControl {
    Begin,
    Commit,
    Rollback,
}

/// The transaction control of each statement of `sql` (statements separated by `;`, outside of
/// quotes, comments and PostgreSQL `$tag$` strings), `None` for any other statement.
/// `ROLLBACK TO SAVEPOINT`, `COMMIT AND CHAIN` and `COMMIT PREPARED` do not end the transaction.
/// Implicit commits (MySQL DDL) are not seen: a transaction is then still taken as open, which
/// only matters if its connection is lost (the next call asks for a `ROLLBACK`).
pub(crate) fn tx_controls(sql: &str) -> Vec<Option<TxControl>> {
    split_statements(sql)
        .into_iter()
        .map(statement_control)
        .collect()
}

fn statement_control(stmt: &str) -> Option<TxControl> {
    let words: Vec<String> = stmt
        .split(|c: char| c.is_whitespace() || c == '(')
        .filter(|w| !w.is_empty())
        .take(4)
        .map(|w| w.to_ascii_uppercase())
        .collect();
    let first = words.first()?.as_str();
    let rest = &words[1..];
    let has = |w: &str| rest.iter().any(|x| x == w);
    match first {
        "BEGIN" => Some(TxControl::Begin),
        "START" if rest.first().is_some_and(|w| w == "TRANSACTION") => Some(TxControl::Begin),
        "COMMIT" | "END" if !has("CHAIN") && !has("PREPARED") => Some(TxControl::Commit),
        "PREPARE" if rest.first().is_some_and(|w| w == "TRANSACTION") => Some(TxControl::Commit),
        "ROLLBACK" | "ABORT" if !has("TO") && !has("CHAIN") && !has("PREPARED") => {
            Some(TxControl::Rollback)
        }
        _ => None,
    }
}

/// The non-empty statements of `sql`, without the comments before them.
fn split_statements(sql: &str) -> Vec<&str> {
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
    while i < bytes.len() {
        match bytes[i] {
            q @ (b'\'' | b'"' | b'`') => {
                // a doubled quote inside is two strings in a row: same result
                i = skip_to(i + 1, &[q]);
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_to(i + 2, b"\n"),
            b'#' => i = skip_to(i + 1, b"\n"),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_to(i + 2, b"*/"),
            b'$' => {
                // PostgreSQL dollar quoting: `$$...$$` or `$tag$...$tag$`, not a `$1` parameter
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
        .map(strip_leading_comments)
        .filter(|s| !s.is_empty())
        .collect()
}

fn strip_leading_comments(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(rest) = s.strip_prefix("--").or_else(|| s.strip_prefix('#')) {
            s = rest.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = rest.split_once("*/").map_or("", |(_, r)| r);
        } else {
            return s.trim_end();
        }
    }
}

/// Whether a transaction is open after `controls` ran on a connection where `in_tx` told it
/// before. `ok` tells whether the statements succeeded: after a failure the statements after
/// the failing one did not run, so a transaction taken as open stays so, unless the one
/// statement was a `COMMIT`/`ROLLBACK` (a failed `COMMIT` ends the transaction in PostgreSQL).
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
pub(crate) struct SessionPool<C> {
    idle: ConnPool<Session<C>>,
    lost: Mutex<HashSet<(ThreadId, String)>>,
}

/// How a [`SessionPool`] runs statements on connections of type `C` with errors of type `E`.
pub(crate) trait Driver<C, E> {
    fn connect(&self, key: &str) -> Result<C, String>;
    /// whether `e` means the connection itself is unusable, rather than the statement failing
    fn is_broken(&self, conn: &C, e: &E) -> bool;
    fn error_text(&self, e: E) -> String;
}

impl<C> SessionPool<C> {
    pub(crate) fn new() -> Self {
        SessionPool {
            idle: ConnPool::per_thread(),
            lost: Mutex::new(HashSet::new()),
        }
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
        let controls = tx_controls(sql);
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
        session.in_tx = tx_open_after(session.in_tx, controls, result.is_ok());
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
        use TxControl::*;
        assert_eq!(tx_controls("BEGIN"), [Some(Begin)]);
        assert_eq!(tx_controls(" start  transaction read only"), [Some(Begin)]);
        assert_eq!(tx_controls("commit work;"), [Some(Commit)]);
        assert_eq!(tx_controls("END"), [Some(Commit)]);
        assert_eq!(tx_controls("abort"), [Some(Rollback)]);
        assert_eq!(tx_controls("ROLLBACK TO SAVEPOINT s1"), [None]);
        assert_eq!(tx_controls("rollback to s1"), [None]);
        assert_eq!(tx_controls("COMMIT AND CHAIN"), [None]);
        assert_eq!(tx_controls("COMMIT PREPARED 'x'"), [None]);
        assert_eq!(tx_controls("select 'BEGIN'"), [None]);
        assert_eq!(
            tx_controls("BEGIN; insert into t values ('a;commit'); -- x;\n COMMIT;"),
            [Some(Begin), None, Some(Commit)]
        );
        assert_eq!(
            tx_controls("/* c */ begin; create function f() as $$ begin; end $$; select $1"),
            [Some(Begin), None, None]
        );
        assert_eq!(tx_controls(" ; ;"), []);
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
    }

    #[test]
    fn test_session_pool_lost_transaction() {
        let pool: SessionPool<u32> = SessionPool::new();
        let fake = Fake {
            next: std::cell::Cell::new(0),
        };
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
