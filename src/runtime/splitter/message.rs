//! Message input: each message of a NATS subject or a Kafka topic is read as an input file of its
//! own, in the input format (`-i`): FNR restarts at 1, FILENAME is the subject (the topic), and with
//! `-H` every CSV/TSV payload starts with its header (the header of a JSON payload comes from its
//! keys). `MSG_SUBJECT`, `MSG_HEADERS` and `MSG_BODY` hold the message being read.
//!
//! The input never ends by itself: SIGINT (Ctrl+C) ends it, then END runs and zawk exits. The
//! SIGINT is only seen while waiting for a message: BEGIN or a rule that is busy keeps running
//! until it is done, and a second Ctrl+C exits at once, without END.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use super::{LineReader, ReaderState, input_warning};
use crate::common::Result;
use crate::pushdown::FieldSet;
use crate::runtime::{Int, RegexCache, Str, set_errno};

/// Where the messages come from: a NATS subject or a Kafka topic.
pub trait MessageSource: Send {
    /// The name of the service, for warnings.
    fn service(&self) -> &'static str;

    /// The next message, or None once SIGINT was received (or the subscription is closed).
    fn next(&mut self) -> Option<Received>;
}

/// A message as it was received.
pub struct Received {
    pub subject: String,
    // Repeated headers are joined with ", ".
    pub headers: Vec<(String, String)>,
    pub payload: Vec<u8>,
}

/// The message being read.
pub(crate) struct Message {
    pub subject: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

// The message being read, for MSG_SUBJECT, MSG_HEADERS and MSG_BODY (see
// `Variables::load_message`).
static CURRENT: Mutex<Option<Arc<Message>>> = Mutex::new(None);

pub(crate) fn current_message() -> Option<Arc<Message>> {
    CURRENT.lock().ok().and_then(|m| m.clone())
}

pub(super) static INTERRUPTED: AtomicBool = AtomicBool::new(false);
// Waiting for a message: a SIGINT ends the input at once.
pub(super) static WAITING: AtomicBool = AtomicBool::new(false);

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
pub(super) fn catch_sigint() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
}

/// The reader of one message: its payload and subject.
type MakeReader<R> = Box<dyn FnMut(Vec<u8>, String) -> R + Send>;

/// Reads the messages of a source, each with a reader `R` of the input format.
pub struct MessageReader<R> {
    source: Box<dyn MessageSource>,
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

impl<R: LineReader> MessageReader<R> {
    pub fn new(
        source: Box<dyn MessageSource>,
        json_document: bool,
        check_utf8: bool,
        make: impl FnMut(Vec<u8>, String) -> R + Send + 'static,
    ) -> Self {
        MessageReader {
            source,
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
        let Some(Received {
            subject,
            headers,
            mut payload,
        }) = self.source.next()
        else {
            self.done = true;
            return false;
        };
        let service = self.source.service();
        // ERRNO tells why the payload of this message could not be parsed
        set_errno(String::new());
        if self.check_utf8 && std::str::from_utf8(&payload).is_err() {
            message_warning(service, &subject, "invalid utf8");
            payload = String::from_utf8_lossy(&payload).into_owned().into_bytes();
        }
        let body = String::from_utf8_lossy(&payload).into_owned();
        if self.json_document {
            payload = match super::json::to_jsonl(&payload, None) {
                Ok(jsonl) => jsonl,
                Err(e) => {
                    message_warning(service, &subject, &format!("invalid JSON: {}", e));
                    // a single record of plain text: `$0` is the payload as is
                    let mut text = serde_json::to_string(&body)
                        .unwrap_or_default()
                        .into_bytes();
                    text.push(b'\n');
                    text
                }
            };
        }
        if let Ok(mut current) = CURRENT.lock() {
            *current = Some(Arc::new(Message {
                subject: subject.clone(),
                headers,
                body,
            }));
        }
        let mut reader = (self.make)(payload, subject);
        reader.set_used_fields(&self.used_fields);
        self.cur = Some(reader);
        self.fresh = true;
        true
    }
}

/// Report a payload that cannot be parsed: a warning, and ERRNO.
fn message_warning(service: &str, subject: &str, msg: &str) {
    use std::sync::atomic::AtomicUsize;
    static INVALID: AtomicUsize = AtomicUsize::new(0);
    input_warning(
        &INVALID,
        || format!("{} message of {}", service, subject),
        msg,
    );
    crate::runtime::stats::rejected_record();
    set_errno(msg.to_string());
}

impl<R: LineReader + 'static> LineReader for MessageReader<R> {
    type Line = R::Line;
    fn filename(&self) -> Str<'static> {
        self.cur
            .as_ref()
            .map(LineReader::filename)
            .unwrap_or_default()
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
