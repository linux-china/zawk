//! NATS subject input: `zawk 'MSG_SUBJECT ~ /demo/ { print }' 'nats://localhost:4222/logs.>'`.
//!
//! The subject may contain wildcards (`orders.*.created`, `orders.>`). Each message is read as an
//! input file of its own, in the input format (`-i`): FNR restarts at 1, FILENAME is the subject,
//! and with `-H` every CSV/TSV payload starts with its header (the header of a JSON payload comes
//! from its keys). `MSG_SUBJECT`, `MSG_HEADERS` and `MSG_BODY` hold the message being read.
//!
//! The input never ends by itself: SIGINT (Ctrl+C) ends it, then END runs and zawk exits. The
//! SIGINT is only seen while waiting for a message: BEGIN or a rule that is busy keeps running
//! until it is done, and a second Ctrl+C exits at once, without END.
//!
//! `?queue=name` subscribes as a member of the queue group `name`: each message goes to one member.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use url::Url;

use super::{input_warning, LineReader, ReaderState};
use crate::common::Result;
use crate::pushdown::FieldSet;
use crate::runtime::network::{nats_client, redact_url, NATS_RUNTIME};
use crate::runtime::{set_errno, Int, RegexCache, Str};

/// Whether `path` is a NATS subject to read messages from.
pub fn is_nats_url(path: &str) -> bool {
    path.starts_with("nats://") || path.starts_with("nats+tls://")
}

/// A message received from the subscription.
pub(crate) struct Message {
    pub subject: String,
    // Repeated headers are joined with ", ".
    pub headers: Vec<(String, String)>,
    pub body: String,
}

// The message being read, for MSG_SUBJECT, MSG_HEADERS and MSG_BODY (see
// `Variables::load_message`).
static CURRENT: Mutex<Option<Arc<Message>>> = Mutex::new(None);

pub(crate) fn current_message() -> Option<Arc<Message>> {
    CURRENT.lock().ok().and_then(|m| m.clone())
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
// Waiting for a message: a SIGINT ends the input at once.
static WAITING: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_sigint(_: libc::c_int) {
    // A second Ctrl+C exits at once, e.g. when BEGIN, a rule or END does not finish.
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        unsafe { libc::_exit(130) };
    }
    if WAITING.load(Ordering::SeqCst) {
        return;
    }
    // only async-signal-safe calls here: tell why nothing happens while the program is busy
    const HINT: &[u8] =
        b"zawk: interrupted: END runs once the current message is processed (Ctrl+C again to exit now)\n";
    unsafe { libc::write(libc::STDERR_FILENO, HINT.as_ptr().cast(), HINT.len()) };
}

/// Make SIGINT end the input rather than the process.
fn catch_sigint() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

/// A subscription to the subject of a `nats://[user:password@]host[:port]/subject[?queue=name]` URL.
pub struct Subscription {
    // Only None once dropped.
    subscriber: Option<async_nats::Subscriber>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // dropping a subscriber unsubscribes on the runtime of the client
        let _guard = NATS_RUNTIME.enter();
        drop(self.subscriber.take());
    }
}

impl Subscription {
    pub fn new(url: &str) -> std::result::Result<Subscription, String> {
        let parsed = Url::parse(url).map_err(|e| format!("invalid URL {:?}: {}", redact_url(url), e))?;
        if parsed.host().is_none() {
            return Err(format!("missing host in {:?}", redact_url(url)));
        }
        // `>` (and spaces) are percent-encoded in the path of a URL
        let path = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
        let subject = urlencoding::decode(path).map_err(|e| e.to_string())?.into_owned();
        if subject.is_empty() {
            return Err(format!("missing subject in {:?}", redact_url(url)));
        }
        let mut queue = None;
        for (name, value) in parsed.query_pairs() {
            match &*name {
                "queue" if !value.is_empty() => queue = Some(value.into_owned()),
                _ => return Err(format!("unsupported query parameter {:?} in {:?}", name, redact_url(url))),
            }
        }
        let conn_url = parsed[..url::Position::BeforePath].replacen("nats+tls://", "tls://", 1);
        let client = nats_client(&conn_url)?;
        let subscriber = NATS_RUNTIME.block_on(async {
            let subscriber = match queue {
                Some(queue) => client.queue_subscribe(subject.clone(), queue).await,
                None => client.subscribe(subject.clone()).await,
            }
                .map_err(|e| format!("failed to subscribe to {:?}: {}", subject, e))?;
            // the server knows about the subscription once the SUB is flushed
            client.flush().await.map_err(|e| format!("failed to subscribe to {:?}: {}", subject, e))?;
            Ok::<_, String>(subscriber)
        })?;
        catch_sigint();
        Ok(Subscription { subscriber: Some(subscriber) })
    }

    /// The next message, or None once SIGINT was received (or the subscription is closed).
    fn next(&mut self) -> Option<async_nats::Message> {
        loop {
            if INTERRUPTED.load(Ordering::SeqCst) {
                return None;
            }
            let subscriber = self.subscriber.as_mut()?;
            WAITING.store(true, Ordering::SeqCst);
            let next = NATS_RUNTIME
                .block_on(async { tokio::time::timeout(Duration::from_millis(100), subscriber.next()).await });
            WAITING.store(false, Ordering::SeqCst);
            match next {
                Ok(Some(msg)) => return Some(msg),
                Ok(None) => {
                    eprintln_ignore!("zawk: warning: NATS subscription closed");
                    return None;
                }
                Err(_elapsed) => continue,
            }
        }
    }
}

/// The reader of one message: its payload and subject.
type MakeReader<R> = Box<dyn FnMut(Vec<u8>, String) -> R + Send>;

/// Reads the messages of a subscription, each with a reader `R` of the input format.
pub struct NatsReader<R> {
    subscription: Subscription,
    make: MakeReader<R>,
    // The payload is a JSON document (`-i json`), converted to JSON Lines for `R`.
    json_document: bool,
    check_utf8: bool,
    // The reader of the message being read.
    cur: Option<R>,
    // The next record is the first one of `cur`: a new "file" starts.
    fresh: bool,
    used_fields: FieldSet,
    done: bool,
}

impl<R: LineReader> NatsReader<R> {
    pub fn new(
        subscription: Subscription,
        json_document: bool,
        check_utf8: bool,
        make: impl FnMut(Vec<u8>, String) -> R + Send + 'static,
    ) -> Self {
        NatsReader {
            subscription,
            make: Box::new(make),
            json_document,
            check_utf8,
            cur: None,
            fresh: false,
            used_fields: FieldSet::all(),
            done: false,
        }
    }

    /// Wait for the next message and start reading it. False once the input is over.
    fn next_message(&mut self) -> bool {
        let Some(msg) = self.subscription.next() else {
            self.done = true;
            return false;
        };
        let subject = msg.subject.to_string();
        let headers = msg
            .headers
            .as_ref()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, values)| {
                        let values: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                        (name.to_string(), values.join(", "))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // ERRNO tells why the payload of this message could not be parsed
        set_errno(String::new());
        let mut payload = msg.payload.to_vec();
        if self.check_utf8 && std::str::from_utf8(&payload).is_err() {
            message_warning(&subject, "invalid utf8");
            payload = String::from_utf8_lossy(&payload).into_owned().into_bytes();
        }
        let body = String::from_utf8_lossy(&payload).into_owned();
        if self.json_document {
            payload = match super::json::to_jsonl(&payload, None) {
                Ok(jsonl) => jsonl,
                Err(e) => {
                    message_warning(&subject, &format!("invalid JSON: {}", e));
                    // a single record of plain text: `$0` is the payload as is
                    let mut text = serde_json::to_string(&body).unwrap_or_default().into_bytes();
                    text.push(b'\n');
                    text
                }
            };
        }
        if let Ok(mut current) = CURRENT.lock() {
            *current = Some(Arc::new(Message { subject: subject.clone(), headers, body }));
        }
        let mut reader = (self.make)(payload, subject);
        reader.set_used_fields(&self.used_fields);
        self.cur = Some(reader);
        self.fresh = true;
        true
    }
}

/// Report a payload that cannot be parsed: a warning, and ERRNO.
fn message_warning(subject: &str, msg: &str) {
    use std::sync::atomic::AtomicUsize;
    static INVALID: AtomicUsize = AtomicUsize::new(0);
    input_warning(&INVALID, || format!("NATS message of {}", subject), msg);
    crate::runtime::stats::rejected_record();
    set_errno(msg.to_string());
}

impl<R: LineReader + 'static> LineReader for NatsReader<R> {
    type Line = R::Line;
    fn filename(&self) -> Str<'static> {
        self.cur.as_ref().map(LineReader::filename).unwrap_or_default()
    }
    fn check_utf8(&self) -> bool {
        self.check_utf8
    }
    fn read_line(&mut self, pat: &Str, rc: &mut RegexCache) -> Result<(bool, R::Line)> {
        let mut line = R::Line::default();
        let changed = self.read_line_reuse(pat, rc, &mut line)?;
        Ok((changed, line))
    }
    fn read_line_reuse<'a, 'b: 'a>(
        &'b mut self,
        pat: &Str,
        rc: &mut RegexCache,
        old: &'a mut Self::Line,
    ) -> Result<bool> {
        loop {
            if let Some(cur) = self.cur.as_mut() {
                let changed = cur.read_line_reuse(pat, rc, old)?;
                if cur.read_state() != ReaderState::Eof as i64 {
                    let changed = changed || self.fresh;
                    self.fresh = false;
                    return Ok(changed);
                }
                // the message is read (an empty payload has no records)
                self.cur = None;
            }
            if self.done || !self.next_message() {
                *old = Default::default();
                return Ok(false);
            }
        }
    }
    fn read_state(&self) -> i64 {
        match &self.cur {
            _ if self.done => ReaderState::Eof as i64,
            Some(cur) => cur.read_state(),
            None => ReaderState::OK as i64,
        }
    }
    fn next_file(&mut self) -> Result<bool> {
        self.cur = None;
        Ok(!self.done)
    }
    fn set_used_fields(&mut self, used_fields: &FieldSet) {
        self.used_fields = used_fields.clone();
        if let Some(cur) = self.cur.as_mut() {
            cur.set_used_fields(used_fields);
        }
    }
    fn new_columns(&mut self, f: &mut dyn FnMut(Str<'static>, Int)) {
        if let Some(cur) = self.cur.as_mut() {
            cur.new_columns(f);
        }
    }
}

/// The payload of a message as the input of a reader.
pub fn payload_reader(payload: Vec<u8>) -> Box<dyn io::Read + Send> {
    Box::new(io::Cursor::new(payload))
}

/// The chunk size of the reader of a payload of `len` bytes: the whole payload, up to `chunk_size`.
pub fn chunk_size(chunk_size: usize, len: usize) -> usize {
    chunk_size.min(len.max(1023) + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_nats_url() {
        assert!(is_nats_url("nats://127.0.0.1:14222/logs.>"));
        assert!(is_nats_url("nats+tls://host/orders.*.created"));
        assert!(!is_nats_url("demo.txt"));
    }

    #[test]
    fn test_missing_subject() {
        assert!(Subscription::new("nats://127.0.0.1:14222").err().unwrap().contains("missing subject"));
        assert!(Subscription::new("nats://127.0.0.1:14222/").err().unwrap().contains("missing subject"));
    }

    #[test]
    fn test_unsupported_query() {
        let err = Subscription::new("nats://127.0.0.1:14222/logs?group=a").err().unwrap();
        assert!(err.contains("unsupported query parameter \"group\""), "{}", err);
        let err = Subscription::new("nats://127.0.0.1:14222/logs?queue=").err().unwrap();
        assert!(err.contains("unsupported query parameter \"queue\""), "{}", err);
    }

    #[test]
    fn test_read_messages() {
        use crate::runtime::splitter::regex::RegexSplitter;
        let subscription = Subscription::new("nats://127.0.0.1:14222/zawk.test.%3E").unwrap();
        let mut reader = NatsReader::new(subscription, false, false, |payload, subject| {
            RegexSplitter::new(payload_reader(payload), chunk_size(1 << 20, 0), subject, false)
        });
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.a", "a1\na2");
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.empty", "");
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.b.c", "b1");
        let mut rc = RegexCache::default();
        let mut records = Vec::new();
        for _ in 0..3 {
            let (changed, line) = reader.read_line(&Str::from("\n"), &mut rc).unwrap();
            records.push((changed, reader.filename().to_string(), line.line.to_string()));
            assert_eq!(current_message().unwrap().subject, reader.filename().to_string());
        }
        assert_eq!(
            records,
            [
                (true, "zawk.test.a".to_string(), "a1".to_string()),
                (false, "zawk.test.a".to_string(), "a2".to_string()),
                (true, "zawk.test.b.c".to_string(), "b1".to_string()),
            ]
        );
    }
}
