//! Connection caches shared by the database, KV and messaging functions, keyed by URL or path.
//!
//! The global lock of a cache is only held to look up an entry: connecting and running
//! statements happen outside of it, so with `-p` a slow query or connection to one URL does
//! not block calls to other URLs.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ThreadId};

/// Idle connections by URL and by the thread that last used them. A caller takes a connection
/// out (or creates one when none is idle), uses it without any lock held, and gives it back, so
/// parallel calls to the same URL run on separate connections.
///
/// A thread only ever gets back its own connections: session state such as an open transaction
/// (`BEGIN` in one call, `COMMIT` in a later one), `SET` variables or temporary tables stays with
/// the worker that created it, and is never picked up by another worker running with `-p`.
pub(crate) struct ConnPool<C> {
    idle: Mutex<HashMap<(ThreadId, String), Vec<C>>>,
}

impl<C> ConnPool<C> {
    pub(crate) fn new() -> Self {
        ConnPool {
            idle: Mutex::new(HashMap::new()),
        }
    }

    /// An idle connection for `key` last used by the current thread, removed from the pool until
    /// it is given back with `put`.
    pub(crate) fn take(&self, key: &str) -> Option<C> {
        let owner = (thread::current().id(), key.to_string());
        self.idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(&owner)
            .and_then(Vec::pop)
    }

    /// Give back a connection that is still usable, kept for the current thread.
    pub(crate) fn put(&self, key: &str, conn: C) {
        let owner = (thread::current().id(), key.to_string());
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
        let pool: Arc<ConnPool<i32>> = Arc::new(ConnPool::new());
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
