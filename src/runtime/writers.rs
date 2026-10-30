//! Writing data to output files
//!
//! This module has the following goals:
//!
//! 1. Support writes to a single file from multiple threads.
//! 2. Support batching writes both within a single thread and also across threads.
//!
//! Supporting all of these requirements, along with the ability to inject fakes for the file
//! system, leads to a fairly involved implementation. Callers build a Registry, which acts as a
//! thread-local cache of a shared mapping from file name to file handle. The core of the
//! implementation is in the implementation of these handles.
//!
//! File handles are each "clients" to a single thread issuing writes on their behalf. That thread
//! reads requests to write, flush, or even close that file and issues them in order. When the
//! thread receives adjacent write requests, it uses the write_vectored API to issue all of those
//! writes at once. This grants us nice batching semantics a la BufWriter without the additional
//! copies.
//!
//! A writer thread only runs while its file is open: it is started by the first write and exits
//! when the file is closed (see `WriterSlot`), so programs that write to many files and `close`
//! them do not accumulate threads.
//!
//! Output files that are open are tracked in an `FdPool`. When opening a file fails because the
//! process ran out of file descriptors, or when too many writer threads run already (see
//! `max_open_files`), the least recently used open file is closed and its writer thread exits; the
//! file is reopened for appending on its next write, as gawk does in `close_one`.
//!
//! Within a client, we batch writes similar to how a BufWriter would: copy incoming writes to a
//! local vector until we have buffered up to a given threshold. Once that threshold is reached, we
//! then send a reference to buffer on the channel to the receiving thread. While this write is
//! pending, the vector is kept alive in a separate buffer of "guards" that also contain an
//! ErrorCode which the receiver thread can use to signal that the pending write has been issued.
//! Buffers corresponding to writes that have completed are reused for future batches. This
//! protocol (as opposed to one that transfers ownership of the buffer to the thread performing the
//! writes) allows each client thread to avoid allocating new buffers continuously. It also
//! mitigates a "producer-consumer" allocation and freeing pattern, which can put a lot of strain
//! on some allocators.
//!
//! To facilitate easier testing, the functionality of the file system that we use is abstracted in
//! the `FileFactory` trait. The `testing` module contains an implementation of this trait that
//! writes all data in memory.

use std::io::IsTerminal;
use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, ChildStdin};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc, Mutex, Once, OnceLock, Weak,
};

#[cfg(not(feature = "unstable"))]
use itertools::Itertools;

#[cfg(not(feature = "unstable"))]
use std::ops::Deref;

// TODO: get_handle() should return an error on failure to parse UTF8

// NB we only require mpsc semantics, but at time of writing there are a few open bugs on
// std::sync::mpsc, while crossbeam_channel is seeing more attention.
use crossbeam_channel::{bounded, Receiver, Sender};
use hashbrown::HashMap;

use crate::common::{CompileError, FileSpec, Notification, Result};
use crate::runtime::str_escape::is_tsv_null;
use crate::runtime::{
    command::{command_env, command_for_write, exit_status_code, CommandEnv},
    Int, Str,
};

/// The maximum number of pending requests in the per-file channels.
const IO_CHAN_SIZE: usize = 8;

/// The size of client-side batches.
const BUFFER_SIZE: usize = 64 << 10;

/// Set when an error writing output was reported at shutdown; see `exit_code`.
static OUTPUT_FAILED: AtomicBool = AtomicBool::new(false);

/// Record that writing some output failed, so that the program does not exit successfully.
pub fn set_output_failed() {
    OUTPUT_FAILED.store(true, Ordering::Release);
}

/// Set when stdout is line buffered even if it is not a terminal; see `set_line_buffered`.
static LINE_BUFFERED: AtomicBool = AtomicBool::new(false);

/// Write each line printed to stdout at once, as for a terminal: the output of a NATS message is
/// not held back until more messages arrive.
pub fn set_line_buffered() {
    LINE_BUFFERED.store(true, Ordering::Relaxed);
}

/// Set when writing to stdout failed because its reader went away (e.g. `zawk ... | head`).
static STDOUT_BROKEN_PIPE: AtomicBool = AtomicBool::new(false);

/// The exit status of a process killed by SIGPIPE, as seen by the shell (128 + 13).
const SIGPIPE_EXIT_CODE: i32 = 141;

/// Whether writing to stdout failed because its reader went away. Such an error is not
/// reported: gawk is quietly killed by SIGPIPE in that case.
pub fn stdout_broken_pipe() -> bool {
    STDOUT_BROKEN_PIPE.load(Ordering::Acquire)
}

/// The exit code to use in place of `code`: as in gawk, a program whose output could not be
/// written exits with code 2 rather than 0, and one whose stdout was closed by its reader exits
/// as if killed by SIGPIPE.
pub fn exit_code(code: i32) -> i32 {
    // Every exit of a program run goes through here, after its output has been flushed.
    crate::runtime::report_suppressed_warnings();
    let code = if stdout_broken_pipe() {
        SIGPIPE_EXIT_CODE
    } else if code == 0 && OUTPUT_FAILED.load(Ordering::Acquire) {
        2
    } else {
        code
    };
    crate::runtime::stats::report(code);
    code
}

/// Whether `e` means that the process (or system) ran out of file descriptors.
fn is_fd_exhausted(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE))
    }
    #[cfg(windows)]
    {
        // ERROR_TOO_MANY_OPEN_FILES
        e.raw_os_error() == Some(4)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// The maximum number of output files whose writer threads run at once. Each running writer
/// holds a thread and a file descriptor, so programs writing to many files without closing them
/// (`print > ($1 ".txt")`) would otherwise run out of threads (and abort). Starting another writer
/// past this limit first closes the least recently used file (see `FdPool::make_room`).
fn max_open_files() -> usize {
    const MAX: usize = 1024;
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        #[cfg(unix)]
        {
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0 {
                // Leave descriptors for input files, commands, getline and the like.
                let cur = usize::try_from(rl.rlim_cur).unwrap_or(usize::MAX);
                return cur.saturating_sub(64).clamp(16, MAX);
            }
        }
        MAX
    })
}

/// Report that a writer thread could not be started or has died (see `WriterSlot::send`). Write
/// errors otherwise end the program silently, but this one is worth a message (printed once).
fn report_spawn_error(e: &io::Error) -> CompileError {
    static REPORTED: Once = Once::new();
    let err = if e.kind() == io::ErrorKind::BrokenPipe {
        CompileError::new(format!("{}", e))
    } else {
        CompileError::new(format!(
            "too many open outputs: cannot start a writer thread: {}",
            e
        ))
    };
    REPORTED.call_once(|| {
        set_output_failed();
        eprintln!("zawk: fatal: {}", err);
    });
    err
}

/// The output files that are currently open, so that a writer can close the least recently used
/// one when it runs out of file descriptors (like gawk's `close_one`), and the number of writer
/// threads of output files, which is kept near `max_open_files` the same way.
#[derive(Default)]
struct FdPool {
    clock: AtomicU64,
    next_id: AtomicUsize,
    /// The number of running writer threads of output files.
    threads: AtomicUsize,
    /// Open files by id: their last use (a tick of `clock`) and their writer.
    open: Mutex<HashMap<usize, (Arc<AtomicU64>, Weak<WriterSlot>)>>,
}

impl FdPool {
    /// Call `f` to open a file or command, closing other open files while it fails for lack of
    /// file descriptors.
    fn open<W>(&self, mut f: impl FnMut() -> io::Result<W>) -> io::Result<W> {
        loop {
            match f() {
                Err(e) if is_fd_exhausted(&e) => {
                    if !self.evict_one() {
                        return Err(e);
                    }
                }
                res => return res,
            }
        }
    }

    /// Close the least recently used open file. Returns false if no file is open.
    ///
    /// This waits for the file's writer to close it. That cannot deadlock: the caller is not
    /// in the pool while it opens a file, and a writer only waits here while opening.
    fn evict_one(&self) -> bool {
        let victim = {
            let mut open = self.open.lock().unwrap();
            let id = open
                .iter()
                .min_by_key(|(_, (last_used, _))| last_used.load(Ordering::Relaxed))
                .map(|(id, _)| *id);
            match id {
                Some(id) => open.remove(&id).unwrap().1,
                None => return false,
            }
        };
        // If the writer has already shut down, its file is closed anyway.
        if let Some(n) = victim.upgrade().and_then(|slot| slot.evict()) {
            n.1.wait();
        }
        true
    }

    /// Called before starting a writer thread: close the least recently used open file if too
    /// many writer threads run already. Its writer thread then exits.
    fn make_room(&self) {
        if self.threads.load(Ordering::Relaxed) >= max_open_files() {
            self.evict_one();
        }
    }
}

/// A file writer's entry in an `FdPool`, owned by its writer thread.
struct PoolMember {
    pool: Arc<FdPool>,
    id: usize,
    last_used: Arc<AtomicU64>,
    slot: Weak<WriterSlot>,
}

impl PoolMember {
    fn new(pool: Arc<FdPool>, slot: Weak<WriterSlot>) -> PoolMember {
        let id = pool.next_id.fetch_add(1, Ordering::Relaxed);
        pool.threads.fetch_add(1, Ordering::Relaxed);
        PoolMember {
            pool,
            id,
            last_used: Default::default(),
            slot,
        }
    }
    fn touch(&self) {
        let now = self.pool.clock.fetch_add(1, Ordering::Relaxed);
        self.last_used.store(now, Ordering::Relaxed);
    }
    fn register(&self) {
        self.touch();
        let entry = (self.last_used.clone(), self.slot.clone());
        self.pool.open.lock().unwrap().insert(self.id, entry);
    }
    fn unregister(&self) {
        self.pool.open.lock().unwrap().remove(&self.id);
    }
}

impl Drop for PoolMember {
    fn drop(&mut self) {
        self.unregister();
        self.pool.threads.fetch_sub(1, Ordering::Relaxed);
    }
}

/// FileFactory abstracts over the portions of the file system used for the output of a frawk
/// program. It includes "file objects" as well as "stdout", which both implement the io::Write
/// trait.
///
/// The factories themselves must also be Clone and thread-safe, as they are passed to writer
/// threads at construction time.
pub trait FileFactory: Clone + 'static + Send + Sync {
    type Output: io::Write;
    type Stdout: io::Write;
    // TODO: make Child an associated type, to permit better testing
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<Child> {
        command_for_write(cmd, env)
    }
    fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output>;
    // TODO maybe we should support this returning an error.
    fn stdout(&self) -> Self::Stdout;
}

impl<W: io::Write, T: Fn(&Path, FileSpec) -> io::Result<W> + Clone + 'static + Send + Sync>
    FileFactory for T
{
    type Output = W;
    type Stdout = grep_cli::StandardStream;
    fn build(&self, path: &Path, spec: FileSpec) -> io::Result<W> {
        (self)(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        grep_cli::stdout(termcolor::ColorChoice::Auto)
    }
}

type FileWriter = std::fs::File;

fn open_file(path: &Path, spec: FileSpec) -> io::Result<FileWriter> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(matches!(spec, FileSpec::Append))
        .truncate(matches!(spec, FileSpec::Trunc))
        .open(path)?;
    Ok(file)
}

/// The path of the output file name `name`. A file name is a sequence of bytes on Unix, so a name
/// that is not valid UTF-8 (a Latin-1 or GBK file name) is opened by its bytes.
#[cfg(unix)]
fn bytes_path(name: &[u8]) -> std::borrow::Cow<'_, Path> {
    use std::os::unix::ffi::OsStrExt;
    std::borrow::Cow::Borrowed(Path::new(std::ffi::OsStr::from_bytes(name)))
}

#[cfg(not(unix))]
fn bytes_path(name: &[u8]) -> std::borrow::Cow<'_, Path> {
    match String::from_utf8_lossy(name) {
        std::borrow::Cow::Borrowed(s) => std::borrow::Cow::Borrowed(Path::new(s)),
        std::borrow::Cow::Owned(s) => std::borrow::Cow::Owned(s.into()),
    }
}

pub fn default_factory() -> impl FileFactory {
    open_file
}

pub fn factory_from_file(fname: &str) -> io::Result<impl FileFactory + use<>> {
    // Do a test open+truncate of the file.
    let _file = open_file(Path::new(fname), FileSpec::Trunc)?;

    #[derive(Clone)]
    struct FileStdout(String);
    impl FileFactory for FileStdout {
        type Output = FileWriter;
        type Stdout = FileWriter;
        fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output> {
            open_file(path, spec)
        }
        fn stdout(&self) -> Self::Stdout {
            open_file(Path::new(&self.0), FileSpec::Append).expect("failed to open stdout")
        }
    }
    Ok(FileStdout(fname.into()))
}

/// Wraps a factory so that stdout is rendered as a Markdown table (`-o markdown`).
///
/// Records are expected to be tab-separated and newline-terminated, with tabs and newlines
/// inside fields already escaped (as for `-o tsv`). Each line becomes a table row; the first one
/// is the header and is followed by the `| --- |` delimiter row. With `summary` set
/// (`--summary`), the row count and per-column statistics follow the table.
#[derive(Clone)]
pub struct MarkdownFactory<F>(pub F, pub bool);

impl<F: FileFactory> FileFactory for MarkdownFactory<F> {
    type Output = F::Output;
    type Stdout = MarkdownWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        MarkdownWriter::new(self.0.stdout(), self.1)
    }
}

pub struct MarkdownWriter<W: io::Write> {
    inner: W,
    /// The current, not yet newline-terminated, record.
    pending: Vec<u8>,
    header_written: bool,
    /// Collects statistics of the rows (`--summary`).
    summary: Option<Summary>,
}

impl<W: io::Write> MarkdownWriter<W> {
    pub fn new(inner: W, summary: bool) -> MarkdownWriter<W> {
        MarkdownWriter {
            inner,
            pending: Vec::new(),
            header_written: false,
            summary: summary.then(Summary::default),
        }
    }

    fn write_row(&mut self, line: &[u8]) -> io::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(summary) = &mut self.summary {
            summary.add(line);
        }
        let mut row = Vec::with_capacity(line.len() + 8);
        let mut columns = 0;
        for cell in line.split(|b| *b == b'\t') {
            row.extend_from_slice(b"| ");
            for &b in cell {
                if b == b'|' {
                    row.push(b'\\');
                }
                row.push(b);
            }
            row.push(b' ');
            columns += 1;
        }
        row.extend_from_slice(b"|\n");
        if !self.header_written {
            self.header_written = true;
            for _ in 0..columns {
                row.extend_from_slice(b"| --- ");
            }
            row.extend_from_slice(b"|\n");
        }
        self.inner.write_all(&row)
    }
}

impl<W: io::Write> io::Write for MarkdownWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rest = buf;
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            if self.pending.is_empty() {
                self.write_row(&rest[..pos])?;
            } else {
                self.pending.extend_from_slice(&rest[..pos]);
                let line = std::mem::take(&mut self.pending);
                self.write_row(&line)?;
            }
            rest = &rest[pos + 1..];
        }
        self.pending.extend_from_slice(rest);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: io::Write> Drop for MarkdownWriter<W> {
    fn drop(&mut self) {
        // A final record without a trailing newline (e.g. from printf) is still a row.
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let _ = self.write_row(&line);
        }
        if let Some(summary) = self.summary.take() {
            if self.header_written {
                let _ = self.inner.write_all(b"\n");
            }
            let _ = self.inner.write_all(summary.markdown().as_bytes());
        }
        let _ = self.inner.flush();
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;

    #[test]
    fn markdown_table() {
        let mut out = Vec::new();
        {
            let mut w = MarkdownWriter::new(&mut out, false);
            w.write_all(b"name\tage\nAl").unwrap();
            w.write_all(b"ice\t3\nB|ob\t\n").unwrap();
            w.write_all(b"last").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "| name | age |\n| --- | --- |\n| Alice | 3 |\n| B\\|ob |  |\n| last |\n"
        );
    }
}

/// Distinct values are counted up to this many per column (`--summary`).
const SUMMARY_MAX_DISTINCT: usize = 10_000;
/// Longer string minimums and maximums are shortened in a summary.
const SUMMARY_MAX_TEXT: usize = 40;

/// The row count and per-column statistics of `-o table` / `-o markdown` output (`--summary`).
///
/// Records are fed in the tab-separated form of `MarkdownFactory`; the first one is the header.
#[derive(Default)]
struct Summary {
    header: Option<Vec<String>>,
    columns: Vec<ColumnStats>,
    rows: usize,
}

#[derive(Default)]
struct ColumnStats {
    non_empty: usize,
    numeric: usize,
    sum: f64,
    min_num: Option<f64>,
    max_num: Option<f64>,
    min_str: Option<String>,
    max_str: Option<String>,
    distinct: hashbrown::HashSet<String>,
}

impl ColumnStats {
    fn add(&mut self, cell: String) {
        if cell.is_empty() {
            return;
        }
        self.non_empty += 1;
        if let Some(n) = crate::runtime::compare::looks_numeric(cell.as_bytes()) {
            self.numeric += 1;
            self.sum += n;
            self.min_num = Some(self.min_num.map_or(n, |m| m.min(n)));
            self.max_num = Some(self.max_num.map_or(n, |m| m.max(n)));
        }
        if self.min_str.as_ref().is_none_or(|m| cell < *m) {
            self.min_str = Some(cell.clone());
        }
        if self.max_str.as_ref().is_none_or(|m| cell > *m) {
            self.max_str = Some(cell.clone());
        }
        if self.distinct.len() < SUMMARY_MAX_DISTINCT {
            self.distinct.insert(cell);
        }
    }

    fn is_number(&self) -> bool {
        self.non_empty > 0 && self.numeric == self.non_empty
    }

    /// The type, non-empty count, distinct count, min, max, sum and mean of the column.
    fn cells(&self) -> [String; 7] {
        let distinct = if self.distinct.len() >= SUMMARY_MAX_DISTINCT {
            format!("{}+", SUMMARY_MAX_DISTINCT)
        } else {
            self.distinct.len().to_string()
        };
        let text = |s: &Option<String>| s.as_deref().map(shorten).unwrap_or_default();
        if self.is_number() {
            let num = |n: Option<f64>| n.map(format_number).unwrap_or_default();
            let mean = (self.sum / self.non_empty as f64 * 1e6).round() / 1e6;
            [
                "number".into(),
                self.non_empty.to_string(),
                distinct,
                num(self.min_num),
                num(self.max_num),
                format_number(self.sum),
                format_number(mean),
            ]
        } else {
            [
                "string".into(),
                self.non_empty.to_string(),
                distinct,
                text(&self.min_str),
                text(&self.max_str),
                String::new(),
                String::new(),
            ]
        }
    }
}

fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 9007199254740992.0 {
        (n as i64).to_string()
    } else {
        n.to_string()
    }
}

fn shorten(s: &str) -> String {
    if s.chars().count() <= SUMMARY_MAX_TEXT {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(SUMMARY_MAX_TEXT - 1).collect();
        t.push('…');
        t
    }
}

impl Summary {
    fn add(&mut self, line: &[u8]) {
        let cells = line.split(|b| *b == b'\t').map(unescape_tsv);
        let header = match &mut self.header {
            None => {
                self.header = Some(cells.collect());
                return;
            }
            Some(header) => header,
        };
        self.rows += 1;
        for (i, cell) in cells.enumerate() {
            // Extra cells get positional names, as `$N` would number them.
            if i >= header.len() {
                header.push(format!("column{}", i + 1));
            }
            if i >= self.columns.len() {
                self.columns.resize_with(i + 1, Default::default);
            }
            self.columns[i].add(cell);
        }
    }

    fn rows_line(&self) -> String {
        format!("{} row{}\n", self.rows, if self.rows == 1 { "" } else { "s" })
    }

    /// The statistics as records, the first one being the header.
    fn records(&self) -> Vec<Vec<String>> {
        let mut records = vec![["column", "type", "non_empty", "distinct", "min", "max", "sum", "mean"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()];
        let empty = ColumnStats::default();
        for (i, name) in self.header.iter().flatten().enumerate() {
            let stats = self.columns.get(i).unwrap_or(&empty);
            let mut record = vec![name.clone()];
            record.extend(stats.cells());
            records.push(record);
        }
        records
    }

    fn table(&self) -> String {
        let mut out = self.rows_line();
        if self.header.is_some() {
            out.push_str(&crate::inspect::boxed_table(self.records()));
        }
        out
    }

    fn markdown(&self) -> String {
        let mut out = self.rows_line();
        if self.header.is_none() {
            return out;
        }
        out.push('\n');
        for (i, record) in self.records().iter().enumerate() {
            for cell in record {
                out.push_str("| ");
                out.push_str(&cell.replace('|', "\\|").replace('\n', " "));
                out.push(' ');
            }
            out.push_str("|\n");
            if i == 0 {
                out.push_str(&"| --- ".repeat(record.len()));
                out.push_str("|\n");
            }
        }
        out
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn markdown_summary() {
        let mut out = Vec::new();
        {
            let mut w = MarkdownWriter::new(&mut out, true);
            w.write_all(b"name\tage\nAlice\t30\nBob\t5\nB|ob\n").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "| name | age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 5 |\n| B\\|ob |\n\n\
             3 rows\n\n\
             | column | type | non_empty | distinct | min | max | sum | mean |\n\
             | --- | --- | --- | --- | --- | --- | --- | --- |\n\
             | name | string | 3 | 3 | Alice | B\\|ob |  |  |\n\
             | age | number | 2 | 2 | 5 | 30 | 35 | 17.5 |\n"
        );
    }

    #[test]
    fn table_summary() {
        let mut out = Vec::new();
        {
            let mut w = TableWriter::new(&mut out, true);
            w.write_all(b"n\n1\n2\n").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "┌───┐\n\
             │ n │\n\
             ├───┤\n\
             │ 1 │\n\
             │ 2 │\n\
             └───┘\n\n\
             2 rows\n\
             ┌────────┬────────┬───────────┬──────────┬─────┬─────┬─────┬──────┐\n\
             │ column │ type   │ non_empty │ distinct │ min │ max │ sum │ mean │\n\
             ├────────┼────────┼───────────┼──────────┼─────┼─────┼─────┼──────┤\n\
             │ n      │ number │         2 │        2 │   1 │   2 │   3 │  1.5 │\n\
             └────────┴────────┴───────────┴──────────┴─────┴─────┴─────┴──────┘\n"
        );
    }

    #[test]
    fn empty_summary() {
        let mut out = Vec::new();
        drop(TableWriter::new(&mut out, true));
        assert_eq!(String::from_utf8(out).unwrap(), "0 rows\n");
    }
}

/// Wraps a factory so that stdout is rendered as a table with box-drawing borders (`-o table`).
///
/// Records are expected in the same tab-separated form as for `MarkdownFactory`. Column widths
/// depend on every row, so the records are buffered and the table is written when stdout is
/// dropped. With `summary` set (`--summary`), the row count and per-column statistics follow the
/// table.
#[derive(Clone)]
pub struct TableFactory<F>(pub F, pub bool);

impl<F: FileFactory> FileFactory for TableFactory<F> {
    type Output = F::Output;
    type Stdout = TableWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        TableWriter::new(self.0.stdout(), self.1)
    }
}

pub struct TableWriter<W: io::Write> {
    inner: W,
    buf: Vec<u8>,
    summary: bool,
}

impl<W: io::Write> TableWriter<W> {
    pub fn new(inner: W, summary: bool) -> TableWriter<W> {
        TableWriter { inner, buf: Vec::new(), summary }
    }

    fn render(&mut self) -> io::Result<()> {
        let buf = std::mem::take(&mut self.buf);
        let text = String::from_utf8_lossy(&buf);
        let records: Vec<Vec<String>> = text
            .lines()
            .map(|line| line.split('\t').map(|c| unescape_tsv(c.as_bytes())).collect())
            .collect();
        self.inner.write_all(crate::inspect::boxed_table(records).as_bytes())?;
        if self.summary {
            let mut summary = Summary::default();
            for line in text.lines() {
                summary.add(line.as_bytes());
            }
            if summary.header.is_some() {
                self.inner.write_all(b"\n")?;
            }
            self.inner.write_all(summary.table().as_bytes())?;
        }
        self.inner.flush()
    }
}

impl<W: io::Write> io::Write for TableWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<W: io::Write> Drop for TableWriter<W> {
    fn drop(&mut self) {
        let _ = self.render();
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;

    #[test]
    fn boxed_table() {
        let mut out = Vec::new();
        {
            let mut w = TableWriter::new(&mut out, false);
            w.write_all(b"name\tage\nAlice\t30\nBob\t5\nCarol").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "┌───────┬─────┐\n\
             │ name  │ age │\n\
             ├───────┼─────┤\n\
             │ Alice │  30 │\n\
             │ Bob   │   5 │\n\
             │ Carol │     │\n\
             └───────┴─────┘\n"
        );
    }

    #[test]
    fn boxed_table_unescapes_tsv() {
        let mut out = Vec::new();
        {
            let mut w = TableWriter::new(&mut out, false);
            w.write_all(b"a\nC:\\\\dir\n").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "┌────────┐\n\
             │ a      │\n\
             ├────────┤\n\
             │ C:\\dir │\n\
             └────────┘\n"
        );
    }
}

/// Wraps a factory so that stdout is written as JSON Lines (`-o jsonl`).
///
/// Records are expected in the same tab-separated form as for `MarkdownFactory`. The first record
/// is the header: a column name may end with `:number`, `:boolean` or `:string` (the default) to
/// set the JSON type of its values. Every following record becomes one JSON object; a column
/// missing from a record, or a field that is the `--null` text, is `null`.
#[derive(Clone)]
pub struct JsonlFactory<F>(pub F);

impl<F: FileFactory> FileFactory for JsonlFactory<F> {
    type Output = F::Output;
    type Stdout = JsonlWriter<F::Stdout>;
    fn cmd(&self, cmd: &[u8], env: Option<&CommandEnv>) -> io::Result<Child> {
        self.0.cmd(cmd, env)
    }
    fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output> {
        self.0.build(path, spec)
    }
    fn stdout(&self) -> Self::Stdout {
        JsonlWriter::new(self.0.stdout())
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum JsonType {
    String,
    Number,
    Boolean,
}

pub struct JsonlWriter<W: io::Write> {
    inner: W,
    /// The current, not yet newline-terminated, record.
    pending: Vec<u8>,
    /// The JSON-encoded column names and their types, once the header has been read.
    columns: Option<Vec<(String, JsonType)>>,
    /// The column names before JSON encoding, made unique (`a`, `a_1`) as for `-i parquet`.
    names: Vec<String>,
    /// Per column, the number of cells that were not valid for its type and were written as null.
    invalid: Vec<u64>,
}

impl<W: io::Write> JsonlWriter<W> {
    pub fn new(inner: W) -> JsonlWriter<W> {
        JsonlWriter {
            inner,
            pending: Vec::new(),
            columns: None,
            names: Vec::new(),
            invalid: Vec::new(),
        }
    }

    fn write_row(&mut self, line: &[u8]) -> io::Result<()> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let raw_cells: Vec<&[u8]> = line.split(|b| *b == b'\t').collect();
        let cells: Vec<String> = raw_cells.iter().map(|raw| unescape_tsv(raw)).collect();
        let columns = match &mut self.columns {
            None => {
                let mut columns = Vec::with_capacity(cells.len());
                for cell in &cells {
                    let (name, ty) = json_column(cell);
                    crate::runtime::splitter::push_unique_name(&mut self.names, name);
                    columns.push((json_string(self.names.last().unwrap()), ty));
                }
                self.columns = Some(columns);
                return Ok(());
            }
            Some(columns) => columns,
        };
        // Extra cells get positional names, as `$N` would number them.
        for i in columns.len()..cells.len() {
            crate::runtime::splitter::push_unique_name(&mut self.names, &format!("column{}", i + 1));
            columns.push((json_string(self.names.last().unwrap()), JsonType::String));
        }
        let mut row = String::with_capacity(line.len() * 2 + 16);
        row.push('{');
        for (i, (name, ty)) in columns.iter().enumerate() {
            if i > 0 {
                row.push(',');
            }
            row.push_str(name);
            row.push(':');
            match cells.get(i) {
                None => row.push_str("null"),
                Some(_) if is_tsv_null(raw_cells[i]) => row.push_str("null"),
                Some(cell) => {
                    if !json_value(cell, *ty, &mut row) {
                        if self.invalid.len() <= i {
                            self.invalid.resize(i + 1, 0);
                        }
                        self.invalid[i] += 1;
                    }
                }
            }
        }
        row.push_str("}\n");
        self.inner.write_all(row.as_bytes())
    }
}

/// Reverses the TSV escaping of `print`; a NULL field (`\N`) is the `--null` text.
fn unescape_tsv(raw: &[u8]) -> String {
    if is_tsv_null(raw) {
        return crate::runtime::str_escape::null_input().to_string();
    }
    crate::runtime::str_escape::unescape_tsv(&String::from_utf8_lossy(raw)).into_owned()
}

fn json_string(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

/// Splits a header cell into the JSON-encoded key and the type of the column. A suffix that is
/// not a known type is part of the name (`host:port` is a string column named "host:port").
fn json_column(header: &str) -> (&str, JsonType) {
    if let Some((name, ty)) = header.rsplit_once(':') {
        let ty = match ty.trim().to_ascii_lowercase().as_str() {
            "number" => Some(JsonType::Number),
            "boolean" => Some(JsonType::Boolean),
            "string" => Some(JsonType::String),
            _ => None,
        };
        if let Some(ty) = ty {
            return (name, ty);
        }
    }
    (header, JsonType::String)
}

/// Appends `cell` as a JSON value of type `ty`, returning false if it was not valid for the type.
/// As for `-o parquet`, empty numbers are null and numbers that are not numeric (or not finite) are
/// null and invalid; booleans are parsed as for `-o parquet` (empty, "0", "false", "no", "off", ...
/// are false).
fn json_value(cell: &str, ty: JsonType, out: &mut String) -> bool {
    use std::fmt::Write;
    use crate::runtime::compare::looks_numeric;
    match ty {
        JsonType::String => out.push_str(&json_string(cell)),
        JsonType::Number => match looks_numeric(cell.as_bytes()).filter(|n| n.is_finite()) {
            Some(n) if n.fract() == 0.0 && n.abs() < 9007199254740992.0 => {
                let _ = write!(out, "{}", n as i64);
            }
            Some(n) => {
                let _ = write!(out, "{}", n);
            }
            None => {
                out.push_str("null");
                return cell.trim().is_empty();
            }
        },
        JsonType::Boolean => {
            let truthy = crate::runtime::math_util::parse_bool(cell);
            out.push_str(if truthy { "true" } else { "false" });
        }
    }
    true
}

impl<W: io::Write> io::Write for JsonlWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rest = buf;
        while let Some(pos) = rest.iter().position(|b| *b == b'\n') {
            if self.pending.is_empty() {
                self.write_row(&rest[..pos])?;
            } else {
                self.pending.extend_from_slice(&rest[..pos]);
                let line = std::mem::take(&mut self.pending);
                self.write_row(&line)?;
            }
            rest = &rest[pos + 1..];
        }
        self.pending.extend_from_slice(rest);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: io::Write> Drop for JsonlWriter<W> {
    fn drop(&mut self) {
        // A final record without a trailing newline (e.g. from printf) is still a row.
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let _ = self.write_row(&line);
        }
        let _ = self.inner.flush();
        if let Some(columns) = &self.columns {
            for ((name, ty), &n) in columns.iter().zip(&self.invalid).filter(|(_, n)| **n > 0) {
                eprintln!("zawk: jsonl column {} ({:?}): {} invalid value(s) written as null", name, ty, n);
            }
        }
    }
}

#[cfg(test)]
mod jsonl_tests {
    use super::*;

    #[test]
    fn jsonl_rows() {
        let mut out = Vec::new();
        {
            let mut w = JsonlWriter::new(&mut out);
            w.write_all(b"name\tage:number\tok:Boolean\thost:port\n").unwrap();
            w.write_all(b"Al\"ice\t30\t1\ta:1\nBob\t1.5\t0\t\tx\nCa").unwrap();
            w.write_all(b"rol\tabc\tyes\nDan\t 0x10 \t\nFay\t1\tNo\nGus\t2\tfalse\n").unwrap();
            w.write_all(b"Eve\\tX").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                r#"{"name":"Al\"ice","age":30,"ok":true,"host:port":"a:1"}"#, "\n",
                r#"{"name":"Bob","age":1.5,"ok":false,"host:port":"","column5":"x"}"#, "\n",
                r#"{"name":"Carol","age":null,"ok":true,"host:port":null,"column5":null}"#, "\n",
                r#"{"name":"Dan","age":16,"ok":false,"host:port":null,"column5":null}"#, "\n",
                r#"{"name":"Fay","age":1,"ok":false,"host:port":null,"column5":null}"#, "\n",
                r#"{"name":"Gus","age":2,"ok":false,"host:port":null,"column5":null}"#, "\n",
                r#"{"name":"Eve\tX","age":null,"ok":null,"host:port":null,"column5":null}"#, "\n",
            )
        );
    }

    #[test]
    fn jsonl_duplicate_names() {
        let mut out = Vec::new();
        {
            let mut w = JsonlWriter::new(&mut out);
            w.write_all(b"a	a:number	a_1	column5
1	2	3	4	5	6
").unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(r#"{"a":"1","a_1":2,"a_1_1":"3","column5":"4","column5_1":"5","column6":"6"}"#, "\n"),
        );
    }
}

/// Exit status of the most recently closed command behind a handle; see `CmdWriter`.
type StatusSlot = Arc<Mutex<Option<Int>>>;

/// The writer for an output command (`print | "cmd"`). Dropping it closes the command's stdin and
/// waits for the command to exit, recording its exit status in `status`. Writer threads drop their
/// writer when processing a close request, so the status is available once the close completes.
struct CmdWriter {
    stdin: Option<ChildStdin>,
    child: Child,
    status: StatusSlot,
}

impl CmdWriter {
    fn new(mut child: Child, status: StatusSlot) -> CmdWriter {
        CmdWriter {
            stdin: child.stdin.take(),
            child,
            status,
        }
    }
    fn stdin(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().expect("stdin of command is open until drop")
    }
}

impl Write for CmdWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin().write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stdin().flush()
    }
}

impl Drop for CmdWriter {
    fn drop(&mut self) {
        // Close stdin first so that the command sees EOF.
        drop(self.stdin.take());
        let code = match self.child.wait() {
            Ok(status) => exit_status_code(status),
            Err(_) => -1,
        };
        if let Ok(mut slot) = self.status.lock() {
            *slot = Some(code);
        }
    }
}

/// Build a handle whose writer thread opens its output with `f`. Files pass the `FdPool` that
/// they join while open.
///
/// The writer thread is started by the first request and exits once the output is closed (see
/// `WriterSlot`), so that programs writing to many files they `close` do not accumulate threads.
fn build_handle<W: io::Write, F: Fn(FileSpec) -> io::Result<W> + Send + Sync + 'static>(
    f: F,
    is_stdout: bool,
    status: StatusSlot,
    pool: Option<Arc<FdPool>>,
) -> RawHandle {
    let error = Arc::new(Mutex::new(None));
    let open = Arc::new(AtomicBool::new(false));
    let evicted = Arc::new(AtomicBool::new(false));
    let f = Arc::new(f);
    let (thread_error, thread_open, thread_evicted) = (error.clone(), open.clone(), evicted.clone());
    let thread_pool = pool.clone();
    let spawn = move |slot: Weak<WriterSlot>| {
        let (sender, receiver) = bounded(IO_CHAN_SIZE);
        let member = thread_pool
            .clone()
            .map(|pool| PoolMember::new(pool, slot.clone()));
        let (error, open, evicted, f) = (
            thread_error.clone(),
            thread_open.clone(),
            thread_evicted.clone(),
            f.clone(),
        );
        std::thread::Builder::new().spawn(move || {
            let state = WriterState {
                open: &open,
                evicted: &evicted,
                member: member.as_ref(),
                slot: &slot,
            };
            receive_thread(receiver, error, state, &*f, is_stdout)
        })?;
        Ok(sender)
    };
    RawHandle {
        error,
        writer: Arc::new(WriterSlot {
            sender: Mutex::new(None),
            spawn: Box::new(spawn),
            pool,
            open: open.clone(),
            evicted,
        }),
        line_buffer: is_stdout && (io::stdout().is_terminal() || LINE_BUFFERED.load(Ordering::Relaxed)),
        status,
        open,
    }
}

/// The channel to the writer thread of a file or command, shared by all of its handles.
///
/// The thread is started on demand and exits when its output is closed. Clients send requests
/// while holding `sender`'s lock, and the thread only exits after taking that lock and finding no
/// pending request (see `WriterSlot::retire`), so a request is never sent to a thread that exits
/// without handling it.
struct WriterSlot {
    sender: Mutex<Option<Sender<Request>>>,
    spawn: Box<dyn Fn(Weak<WriterSlot>) -> io::Result<Sender<Request>> + Send + Sync>,
    /// The pool of output files, which limits the number of writer threads (unset for commands
    /// and stdout).
    pool: Option<Arc<FdPool>>,
    /// See `RawHandle::open`.
    open: Arc<AtomicBool>,
    /// Whether the file was closed to free its descriptor and thread (see `FdPool`): it is still
    /// open as far as the program is concerned, and the next write reopens it for appending.
    evicted: Arc<AtomicBool>,
}

impl WriterSlot {
    /// Send `req` to the writer thread, starting it for a write if it is not running. Fails if
    /// the thread cannot be started or has died; `req` is then answered with an error.
    fn send(self: &Arc<Self>, req: Request) -> io::Result<()> {
        let mut sender = self.sender.lock().unwrap();
        if sender.is_none() {
            if !matches!(req, Request::Write { .. }) {
                // The output is closed and no request is pending: flushing or closing it is a
                // no-op, not worth starting a thread for. Closing an evicted file closes it for
                // good: it is truncated if reopened by `>`.
                if matches!(req, Request::Close) && self.evicted.swap(false, Ordering::AcqRel) {
                    self.open.store(false, Ordering::Release);
                }
                req.set_code(ErrorCode::set_ok);
                return Ok(());
            }
            if let Some(pool) = &self.pool {
                // Making room waits for another writer to close its file. Do not hold our lock
                // meanwhile: a client making room by evicting this file would wait for it.
                drop(sender);
                pool.make_room();
                sender = self.sender.lock().unwrap();
            }
            if sender.is_none() {
                match (self.spawn)(Arc::downgrade(self)) {
                    Ok(s) => *sender = Some(s),
                    Err(e) => {
                        req.set_code(ErrorCode::set_error);
                        return Err(e);
                    }
                }
            }
        }
        if let Err(e) = sender.as_ref().unwrap().send(req) {
            // The writer thread died (e.g. it panicked): fail the request like an I/O error.
            e.0.set_code(ErrorCode::set_error);
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "output writer thread exited unexpectedly",
            ));
        }
        Ok(())
    }

    /// Ask a running writer thread to close its file to free the descriptor (see `FdPool`).
    /// Returns `None` if no thread is running: its file is closed already.
    fn evict(&self) -> Option<Arc<(ErrorCode, Notification)>> {
        let sender = self.sender.lock().unwrap();
        let sender = sender.as_ref()?;
        let (n, req) = Request::evict();
        match sender.send(req) {
            Ok(()) => Some(n),
            Err(e) => {
                // The writer thread died: release the request.
                e.0.set_code(ErrorCode::set_ok);
                None
            }
        }
    }

    /// Called by the writer thread when its output is closed: returns whether the thread may
    /// exit, which it may when no request is pending. A client sending a request at the same
    /// time holds the lock, so the thread then keeps running to handle the request.
    fn retire(slot: &Weak<WriterSlot>, receiver: &Receiver<Request>) -> bool {
        let Some(slot) = slot.upgrade() else {
            // No handle is left to send requests.
            return true;
        };
        loop {
            if !receiver.is_empty() {
                return false;
            }
            // A client holds the lock only while sending a request (which may block while the
            // channel is full, but then the channel is not empty), so wait for it rather than
            // keep running idle: idle writers would count against `max_open_files`.
            match slot.sender.try_lock() {
                Ok(mut sender) => {
                    if !receiver.is_empty() {
                        return false;
                    }
                    *sender = None;
                    return true;
                }
                Err(std::sync::TryLockError::WouldBlock) => std::thread::yield_now(),
                Err(std::sync::TryLockError::Poisoned(_)) => return false,
            }
        }
    }
}

enum Special {
    Stdout,
    Stderr,
}

/// The output files that gawk treats specially: they write to the process's stdout and stderr
/// rather than opening a new file.
fn special_file(name: &Str) -> Option<Special> {
    name.with_bytes(|bs| match bs {
        b"/dev/stdout" => Some(Special::Stdout),
        b"/dev/stderr" => Some(Special::Stderr),
        _ => None,
    })
}

/// Registry is a thread-local handle on all files we have ever interacted with.
///
/// Note that handles are never removed, even after a file is closed: closing a file again returns
/// -1, as in gawk. A closed file's writer thread exits, and the next write starts a new one.
pub struct Registry {
    global: Arc<dyn Root>,
    files: HashMap<Str<'static>, FileHandle>,
    cmds: HashMap<Str<'static>, FileHandle>,
    stdout: FileHandle,
    /// `/dev/stderr`. Writes to it are synchronous, after flushing stdout, so that output to
    /// stdout and stderr appears in program order.
    stderr: FileHandle,
    /// Whether this is the original registry rather than a clone made for a worker thread. Only
    /// the root waits for output commands at shutdown: workers share commands, and closing one
    /// while other workers still write to it would start the command again.
    is_root: bool,
}

impl Registry {
    pub fn from_factory(f: impl FileFactory) -> Registry {
        let root_impl = RootImpl::from_factory(f);
        let stdout = root_impl.get_stdout().into_handle();
        let stderr = root_impl.get_stderr().into_stderr_handle();
        Registry {
            global: Arc::new(root_impl),
            files: Default::default(),
            cmds: Default::default(),
            stdout,
            stderr,
            is_root: true,
        }
    }

    pub fn get_handle(&mut self, name: Option<&Str>, fspec: FileSpec) -> Result<&mut FileHandle> {
        let name = if let Some(s) = name {
            s
        } else {
            return self.get_file(None);
        };
        match fspec {
            FileSpec::Cmd => self.get_cmd(name),
            FileSpec::Trunc | FileSpec::Append => self.get_file(Some(name)),
        }
    }

    /// Close a file or command. Returns the exit status of a command, 0 for a file, and `None`
    /// if `path_or_cmd` was never opened.
    pub fn close(&mut self, path_or_cmd: &Str) -> Result<Option<Int>> {
        // As in gawk, closing /dev/stdout or /dev/stderr only flushes it.
        match special_file(path_or_cmd) {
            Some(Special::Stdout) => return self.stdout.flush().map(|_| Some(0)),
            Some(Special::Stderr) => return self.stderr.flush().map(|_| Some(0)),
            None => {}
        }
        // TODO: implement a newtype for heterogeneous lookup. We shouldn't have to do the clone or
        // the unmoor here, but we need to because we cannot implement Borrow<Str<'a>> for
        // Borrow<Str<'static>> (conflicts with the blanket impl for Borrow).
        if let Some(fh) = self.files.get_mut(&path_or_cmd.clone().unmoor()) {
            fh.checked = false;
            // The handle outlives a close: closing it again returns -1 (`None`), as in gawk.
            fh.flush()?;
            if fh.raw.is_open() {
                fh.close()?;
                return Ok(Some(0));
            }
        }
        if let Some(mut ch) = self.cmds.remove(&path_or_cmd.clone().unmoor()) {
            // Output already printed should appear before anything the command prints on exit.
            self.stdout.flush()?;
            ch.close()?;
            return Ok(Some(ch.raw.take_status()));
        }
        let is_cmd = path_or_cmd.with_bytes(|bs| self.global.is_command(bs));
        if is_cmd {
            self.stdout.flush()?;
        }
        path_or_cmd.with_bytes(|bs| self.global.close(bs))
    }

    /// Check that the output file `name` can be opened before writing to it, as files are opened
    /// lazily by the writer threads. As in gawk, failing to open an output file is an error
    /// (unlike other write errors, which end the program silently).
    pub fn check_output(&mut self, name: &Str, fspec: FileSpec) -> Result<()> {
        if let FileSpec::Cmd = fspec {
            return Ok(());
        }
        if special_file(name).is_some() {
            return Ok(());
        }
        let global = self.global.clone();
        let fh = self.get_file(Some(name))?;
        if fh.checked {
            return Ok(());
        }
        if let Err(e) = name.with_bytes(|bs| global.check_open(bs)) {
            return Err(CompileError::new(format!("cannot redirect to `{}': {}", name.as_str(), e)));
        }
        fh.checked = true;
        Ok(())
    }

    pub fn get_cmd(&mut self, cmd: &Str) -> Result<&mut FileHandle> {
        use hashbrown::hash_map::Entry;
        // borrowed by with_bytes closure.
        let global = &self.global;
        match self.cmds.entry(cmd.clone().unmoor()) {
            Entry::Occupied(o) => Ok(o.into_mut()),
            Entry::Vacant(v) => {
                // As in gawk, flush pending output before (possibly) starting a command, so that
                // output printed earlier appears before the command's output.
                self.stdout.flush()?;
                Ok(v.insert(cmd.with_bytes(|bs| global.get_command(bs)).into_handle()))
            }
        }
    }

    pub fn get_file(&mut self, name: Option<&Str>) -> Result<&mut FileHandle> {
        match name.map(special_file) {
            Some(Some(Special::Stdout)) => return Ok(&mut self.stdout),
            Some(Some(Special::Stderr)) => {
                // Keep stdout and stderr output in program order.
                if self.stdout.dirty {
                    self.stdout.flush()?;
                }
                return Ok(&mut self.stderr);
            }
            _ => {}
        }
        match name {
            Some(path) => {
                use hashbrown::hash_map::Entry;
                // borrowed by with_bytes closure.
                let global = &self.global;
                match self.files.entry(path.clone().unmoor()) {
                    Entry::Occupied(o) => Ok(o.into_mut()),
                    Entry::Vacant(v) => {
                        let raw = path.with_bytes(|bs| global.get_handle(bs));
                        Ok(v.insert(raw.into_handle()))
                    }
                }
            }
            None => Ok(&mut self.stdout),
        }
    }

    /// awk's `fflush(name)`: flush the output file or command `name`, or all output if `name`
    /// is empty. Returns whether `name` is open for output (/dev/stdout and /dev/stderr always
    /// are).
    pub fn flush_named(&mut self, name: &Str) -> Result<bool> {
        if name.is_empty() {
            self.flush_all()?;
            return Ok(true);
        }
        let key = name.clone().unmoor();
        let mut found = false;
        for fh in [self.files.get_mut(&key), self.cmds.get_mut(&key)].into_iter().flatten() {
            fh.flush()?;
            found |= fh.raw.is_open();
        }
        let special = name.with_bytes(|bs| bs == b"/dev/stdout" || bs == b"/dev/stderr");
        match special_file(name) {
            Some(Special::Stdout) => self.stdout.flush()?,
            Some(Special::Stderr) => self.stderr.flush()?,
            None => {}
        }
        if found || special {
            return Ok(true);
        }
        // The file or command may have been opened by another thread.
        name.with_bytes(|bs| self.global.flush(bs))
    }

    /// Flush all output files, commands and stdout (e.g. before `system`).
    pub fn flush_all(&mut self) -> Result<()> {
        for fh in self.files.values_mut().chain(self.cmds.values_mut()) {
            fh.flush()?;
        }
        self.stdout.flush()
    }

    pub fn destroy_and_flush_all_files(&mut self) -> Result<()> {
        let mut last_error = Ok(());
        for (_, mut fh) in self.files.drain().chain(self.cmds.drain()) {
            let res = fh.flush();
            if res.is_err() {
                last_error = res;
            }
        }
        if self.is_root {
            // Wait for output commands to finish, so that their output is complete when we exit.
            let res = self.global.close_commands();
            if res.is_err() {
                last_error = res;
            }
            // Close (rather than only flush) stdout, so that its writer is dropped before we
            // exit: wrappers such as `ParquetWriter` write their trailer when dropped.
            let res = self.stdout.close();
            // A closed pipe on stdout (e.g. `zawk ... | head`) is not an error worth reporting;
            // it is reflected in the exit code instead (see `exit_code`).
            if res.is_err() && !stdout_broken_pipe() {
                last_error = res;
            }
        }
        last_error
    }
}

impl Clone for Registry {
    fn clone(&self) -> Registry {
        Registry {
            global: self.global.clone(),
            files: Default::default(),
            cmds: Default::default(),
            stdout: self.stdout.raw().into_handle(),
            stderr: self.stderr.raw().into_stderr_handle(),
            is_root: false,
        }
    }
}

// We place Root behind a trait so that we can maintain static dispatch at the level of the
// receiver threads, while still avoiding an extra type parameter all the way up the stack.
trait Root: 'static + Send + Sync {
    fn get_command(&self, cmd: &[u8]) -> RawHandle;
    fn get_handle(&self, fname: &[u8]) -> RawHandle;
    fn get_stdout(&self) -> RawHandle;
    fn get_stderr(&self) -> RawHandle;
    // opens the file `fname` for appending (creating it, never truncating it) to check that it
    // can be written.
    fn check_open(&self, fname: &[u8]) -> io::Result<()>;
    // closes a file or command with name `fname`; see Registry::close for the return value.
    fn close(&self, fname: &[u8]) -> Result<Option<Int>>;
    fn is_command(&self, fname: &[u8]) -> bool;
    // flushes a file or command with name `fname`; returns whether it is open.
    fn flush(&self, fname: &[u8]) -> Result<bool>;
    // closes all commands, waiting for them to exit.
    fn close_commands(&self) -> Result<()>;
}

struct RootImpl<F> {
    handles: Mutex<HashMap<Box<[u8]>, RawHandle>>,
    commands: Mutex<HashMap<Box<[u8]>, RawHandle>>,
    stdout_raw: RawHandle,
    stderr_raw: RawHandle,
    file_factory: F,
    pool: Arc<FdPool>,
}

impl<F: FileFactory> RootImpl<F> {
    fn from_factory(file_factory: F) -> RootImpl<F> {
        let local_factory = file_factory.clone();
        let stdout_raw = build_handle(
            move |_append| Ok(local_factory.stdout()),
            /*is_stdout*/ true,
            Default::default(),
            None,
        );
        // Writes to stderr bypass this handle's writer thread (see FileHandle::direct), it only
        // gives the stderr FileHandle a RawHandle.
        let stderr_raw = build_handle(
            |_append| Ok(io::stderr()),
            /*is_stdout*/ false,
            Default::default(),
            None,
        );
        RootImpl {
            handles: Default::default(),
            commands: Default::default(),
            stdout_raw,
            stderr_raw,
            file_factory,
            pool: Default::default(),
        }
    }
}

impl<F: FileFactory> Root for RootImpl<F> {
    fn is_command(&self, fname: &[u8]) -> bool {
        self.commands.lock().unwrap().contains_key(fname)
    }
    fn flush(&self, fname: &[u8]) -> Result<bool> {
        // Avoid flushing with the locks held.
        let mut handle = self.commands.lock().unwrap().get(fname).cloned();
        if handle.is_none() {
            handle = self.handles.lock().unwrap().get(fname).cloned();
        }
        match handle {
            Some(h) => {
                let mut fh = h.into_handle();
                fh.flush()?;
                Ok(fh.raw.is_open())
            }
            None => Ok(false),
        }
    }
    fn close_commands(&self) -> Result<()> {
        // Avoid calling close with the lock held.
        let handles: Vec<RawHandle> = self.commands.lock().unwrap().values().cloned().collect();
        let mut last_error = Ok(());
        for h in handles {
            if let Err(e) = h.into_handle().close() {
                last_error = Err(e);
            }
        }
        last_error
    }
    fn close(&self, fname: &[u8]) -> Result<Option<Int>> {
        let mut handle = None;
        {
            let cmds = self.commands.lock().unwrap();
            if let Some(h) = cmds.get(fname) {
                // We do this extra song and dance to avoid calling close with the lock held.
                handle = Some(h.clone());
            }
        }
        if let Some(h) = handle.take() {
            let mut fh = h.into_handle();
            fh.flush()?;
            if fh.raw.is_open() {
                fh.close()?;
                return Ok(Some(fh.raw.take_status()));
            }
        }
        {
            let files = self.handles.lock().unwrap();
            if let Some(h) = files.get(fname) {
                handle = Some(h.clone());
            }
        }
        if let Some(h) = handle.take() {
            let mut fh = h.into_handle();
            fh.flush()?;
            if fh.raw.is_open() {
                fh.close()?;
                return Ok(Some(0));
            }
        }
        Ok(None)
    }
    fn get_command(&self, cmd: &[u8]) -> RawHandle {
        let mut cmds = self.commands.lock().unwrap();
        if let Some(h) = cmds.get(cmd) {
            return h.clone();
        }
        let local_factory = self.file_factory.clone();
        let local_name = Box::<[u8]>::from(cmd);
        let global_name = local_name.clone();
        let status = StatusSlot::default();
        let writer_status = status.clone();
        // The command is spawned on the writer thread; take its environment from this one.
        let env = command_env();
        let pool = self.pool.clone();
        let handle = build_handle(
            move |_| {
                pool.open(|| local_factory.cmd(&local_name, env.as_ref()))
                    .map(|child| CmdWriter::new(child, writer_status.clone()))
            },
            /*is_stdout=*/ false,
            status,
            None,
        );
        let _old = cmds.insert(global_name, handle.clone());
        debug_assert!(
            _old.is_none(),
            "duplicate insertion of command: {}",
            String::from_utf8_lossy(cmd)
        );
        handle
    }
    fn get_handle(&self, fname: &[u8]) -> RawHandle {
        let mut handles = self.handles.lock().unwrap();
        if let Some(h) = handles.get(fname) {
            return h.clone();
        }
        let local_factory = self.file_factory.clone();
        let local_name = Box::<[u8]>::from(fname);
        let global_name = local_name.clone();
        let pool = self.pool.clone();
        let handle = build_handle(
            move |spec| pool.open(|| local_factory.build(&bytes_path(&local_name), spec)),
            /*is_stdout=*/ false,
            Default::default(),
            Some(self.pool.clone()),
        );
        handles.insert(global_name, handle.clone());
        handle
    }
    fn get_stdout(&self) -> RawHandle {
        self.stdout_raw.clone()
    }
    fn get_stderr(&self) -> RawHandle {
        self.stderr_raw.clone()
    }
    fn check_open(&self, fname: &[u8]) -> io::Result<()> {
        self.pool
            .open(|| self.file_factory.build(&bytes_path(fname), FileSpec::Append))
            .map(drop)
    }
}

/// FileHandle contains thread-local state around writing to and closing an output file.
pub struct FileHandle {
    raw: RawHandle,
    // Why do we deal with Box<WriteGuard>s and not WriteGuards?
    //
    // We pass a reference to the ErrorCode within a WriteGuard to a write request. That address
    // must remain stable; if we stored a WriteGuard in a VecDeque directly we would not have that
    // guarantee. old_guards caches recent WriteGuards that have been discarded to avoid allocation
    // overheads in cases where we aren't using a fast malloc (in cases where we are, doing this
    // may still be marginally faster).
    #[allow(clippy::vec_box)]
    old_guards: Vec<Box<WriteGuard>>,
    guards: VecDeque<Box<WriteGuard>>,
    cur_batch: Box<WriteGuard>,
    /// Whether opening the file was checked (see Registry::check_output) since it was last
    /// closed.
    checked: bool,
    /// Write synchronously and unbuffered to the process's stderr from the calling thread,
    /// rather than through the writer thread (used for /dev/stderr).
    direct: bool,
    /// Whether anything was written since the last flush.
    dirty: bool,
}

impl FileHandle {
    fn raw(&self) -> RawHandle {
        self.raw.clone()
    }

    fn clear_guards(&mut self) -> Result<()> {
        let mut done_count = 0;
        for (i, guard) in self.guards.iter().enumerate() {
            match guard.status() {
                RequestStatus::Ongoing => break,
                RequestStatus::Ok => done_count = i,
                RequestStatus::Error => return Err(self.read_error()),
            }
        }
        // TODO: off-by-one, should morally be ..=, but that doesn't handle the empty case.
        for _ in 0..done_count {
            let old = self.guards.pop_front().unwrap();
            if self.old_guards.len() < IO_CHAN_SIZE {
                self.old_guards.push(old);
            }
        }
        Ok(())
    }

    fn guard(&mut self) -> Box<WriteGuard> {
        if let Some(mut g) = self.old_guards.pop() {
            g.activate();
            g
        } else {
            Box::<WriteGuard>::default()
        }
    }

    fn read_error(&self) -> CompileError {
        // The receiver shut down before we did. That means something went wrong: probably an IO
        // error of some kind. In that case, the receiver thread stashed away the error it received
        // in raw.error for us to read it out. We don't optimize this path too aggressively because
        // IO errors in frawk scripts are fatal.
        const BAD_SHUTDOWN_MSG: &str = "internal error: (writer?) thread did not shut down cleanly";
        if let Ok(lock) = self.raw.error.lock() {
            match &*lock {
                Some(err) => err.clone(),
                None => CompileError::new(BAD_SHUTDOWN_MSG.into()),
            }
        } else {
            CompileError::new(BAD_SHUTDOWN_MSG.into())
        }
    }

    fn clear_batch(&mut self, upto: Option<usize>) -> Result<()> {
        if !self.cur_batch.pending {
            return Ok(());
        }
        let (flush, upto) = if let Some(ix) = upto {
            (true, ix)
        } else {
            (false, self.cur_batch.data.len())
        };
        self.clear_guards()?;
        let mut next_batch = self.guard();
        self.cur_batch.peel(upto, &mut next_batch);
        let req = self.cur_batch.request(flush);
        let sent = self.raw.writer.send(req);
        std::mem::swap(&mut next_batch, &mut self.cur_batch);
        self.guards.push_back(next_batch);
        sent.map_err(|e| report_spawn_error(&e))
    }

    pub fn write_all(&mut self, ss: &[&Str], spec: FileSpec) -> Result<()> {
        if self.direct {
            let mut err = io::stderr().lock();
            for s in ss.iter() {
                let bs = unsafe { &*s.get_bytes() };
                if let Err(e) = err.write_all(bs) {
                    return Err(CompileError::new(format!("{}", e)));
                }
            }
            return err.flush().map_err(|e| CompileError::new(format!("{}", e)));
        }
        self.dirty = true;
        let cur_len = self.cur_batch.data.len();
        let mut added_bytes = 0;
        let mut last_line = None;
        // Record the write even if every string is empty: `printf "" > f` must still open (and
        // truncate) f.
        self.cur_batch.extend(&[], spec);
        for s in ss.iter() {
            s.for_each_chunk(|bs| {
                self.cur_batch.extend(bs, spec);
                if self.raw.line_buffer {
                    if let Some(ix) = memchr::memchr(b'\n', bs) {
                        // +1 to include the newline
                        last_line = Some(cur_len + added_bytes + ix + 1);
                    }
                }
                added_bytes += bs.len();
            });
        }
        if (self.raw.line_buffer && last_line.is_some()) || (added_bytes + cur_len > BUFFER_SIZE) {
            self.clear_batch(last_line)?;
        }
        Ok(())
    }
    pub fn write(&mut self, s: &Str, spec: FileSpec) -> Result<()> {
        self.write_all(&[s], spec)
    }

    pub fn flush(&mut self) -> Result<()> {
        if self.direct {
            return Ok(());
        }
        // Even if an earlier write failed, wait for the writer thread to get through every
        // pending request before releasing the guards: those requests point into them. After an
        // error the writer thread answers all requests with an error, so the wait terminates.
        let res = self.clear_batch(None);
        let (n, req) = Request::flush();
        // Only writes start a writer thread, so this cannot fail.
        let _ = self.raw.writer.send(req);
        n.1.wait();
        self.guards.clear();
        res?;
        if let RequestStatus::Error = n.0.read() {
            Err(self.read_error())
        } else {
            self.dirty = false;
            Ok(())
        }
    }

    pub fn close(&mut self) -> Result<()> {
        self.clear_batch(None)?;
        let _ = self.raw.writer.send(Request::Close);
        let res = self.flush();
        // Release the batch buffers: a program may close many files it never writes again.
        self.old_guards.clear();
        self.cur_batch.data = Vec::new();
        res
    }
}

impl Drop for FileHandle {
    fn drop(&mut self) {
        let _ = self.flush();
        self.cur_batch.status.set_ok();
    }
}

/// A basic atomic error code type:
///
/// * 0 => "Ongoing"
/// * 1 => "Ok"
/// * 2 => "Error"
///
/// ErrorCode is used in write and flush requests to signal if a request is still pending, has
/// completed successfully, or ran into an error. The "Error" case contains no additional error
/// information. FileHandle sets up a separate channel to transmit the full error; encountering an
/// "Error" status is merely a signal to check this location for a more detailed error message.
#[derive(Default)]
struct ErrorCode(AtomicUsize);

#[derive(Debug)]
enum RequestStatus {
    Ongoing = 0,
    Ok = 1,
    Error = 2,
}

impl ErrorCode {
    fn read(&self) -> RequestStatus {
        match self.0.load(Ordering::Acquire) {
            0 => RequestStatus::Ongoing,
            1 => RequestStatus::Ok,
            2 => RequestStatus::Error,
            _ => unreachable!(),
        }
    }
    fn set_ok(&self) {
        self.0.store(RequestStatus::Ok as usize, Ordering::Release);
    }
    fn set_error(&self) {
        self.0
            .store(RequestStatus::Error as usize, Ordering::Release);
    }
}

/// Request is the "wire protocol" sent from client threads to the writing thread for a given file.
enum Request {
    // Because frawk has no separate call for opening an output file, we pass `append` along with
    // write requests. The append value does nothing unless this is the first write received (after
    // a close).
    Write {
        data: *const [u8],
        status: *const ErrorCode,
        spec: FileSpec,
        flush: bool,
    },
    Flush(Arc<(ErrorCode, Notification)>),
    Close,
    /// Close the file to free its descriptor (see `FdPool`). Unlike `Close`, the file remains
    /// open as far as the program is concerned: the next write reopens it for appending.
    Evict(Arc<(ErrorCode, Notification)>),
}

// This isn't implemented automatically because of the raw pointers in Write. Those pointers are
// never mutated or reassigned, and the protocol guarantees that they remain valid for as long as
// the receiver thread has a reference to them.
unsafe impl Send for Request {}

impl Request {
    fn flush() -> (Arc<(ErrorCode, Notification)>, Request) {
        let notify = Arc::new((ErrorCode::default(), Notification::default()));
        let req = Request::Flush(notify.clone());
        (notify, req)
    }
    fn evict() -> (Arc<(ErrorCode, Notification)>, Request) {
        let notify = Arc::new((ErrorCode::default(), Notification::default()));
        let req = Request::Evict(notify.clone());
        (notify, req)
    }
    fn size(&self) -> usize {
        match self {
            // NB, aside from the invariants we maintain about the validity of `data`, grabbing the
            // length here should _always_ be safe. This is tracked by the {const_}slice_ptr_len
            // feature.
            Request::Write { data, .. } => unsafe { &**data }.len(),
            Request::Flush(_) | Request::Close | Request::Evict(_) => 0,
        }
    }
    fn set_code(&self, mut f: impl FnMut(&ErrorCode)) {
        match self {
            Request::Write { status, .. } => f(unsafe { &**status }),
            Request::Flush(n) | Request::Evict(n) => {
                f(&n.0);
                n.1.notify();
            }
            Request::Close => {}
        }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        match self {
            // Do not turn a panic into an abort.
            _ if std::thread::panicking() => {}
            Request::Write { status, .. } => {
                // We have to have set this as either ok, or an error.
                let status = unsafe { &**status }.read();
                assert!(!matches!(status, RequestStatus::Ongoing));
            }
            Request::Flush(n) | Request::Evict(n) => {
                assert!(n.1.has_been_notified());
            }
            Request::Close => {}
        }
    }
}

/// WriteGuard represents a pending write request.
#[derive(Default)]
struct WriteGuard {
    data: Vec<u8>,
    status: ErrorCode,
    spec: FileSpec,
    /// Whether this guard holds a write, possibly of zero bytes, that is yet to be sent.
    pending: bool,
}

impl WriteGuard {
    fn extend(&mut self, bs: &[u8], spec: FileSpec) {
        // The spec only matters when the batch (re)opens the file, so it is the spec of the
        // batch's first write: in `print "a" > f; print "b" >> f`, the file is still truncated.
        if !self.pending {
            self.spec = spec;
            self.pending = true;
        }
        self.data.extend(bs);
    }

    fn peel(&mut self, bytes: usize, next: &mut WriteGuard) {
        if bytes < self.data.len() {
            next.data.extend(self.data[bytes..].iter().cloned());
            next.pending = true;
            self.data.truncate(bytes);
        }
    }

    fn request(&self, flush: bool) -> Request {
        Request::Write {
            data: &self.data[..],
            status: &self.status,
            spec: self.spec,
            flush,
        }
    }

    fn status(&self) -> RequestStatus {
        self.status.read()
    }

    fn activate(&mut self) {
        self.status = ErrorCode::default();
        self.spec = FileSpec::Trunc;
        self.data.clear();
        self.pending = false;
    }
}

impl Drop for WriteGuard {
    fn drop(&mut self) {
        let status = self.status();
        assert!(!matches!(status, RequestStatus::Ongoing));
    }
}

#[derive(Clone)]
struct RawHandle {
    error: Arc<Mutex<Option<CompileError>>>,
    writer: Arc<WriterSlot>,
    line_buffer: bool,
    /// Set when a command behind this handle exits; unused for files and stdout.
    status: StatusSlot,
    /// Whether the file or command is currently open, as maintained by the writer thread. Up to
    /// date after a flush.
    open: Arc<AtomicBool>,
}

impl RawHandle {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
    /// The exit status of the command closed most recently, or 0 (e.g. for files).
    fn take_status(&self) -> Int {
        self.status.lock().ok().and_then(|mut s| s.take()).unwrap_or(0)
    }
}

impl RawHandle {
    fn into_handle(self) -> FileHandle {
        FileHandle {
            cur_batch: Default::default(),
            raw: self,
            guards: Default::default(),
            old_guards: Default::default(),
            checked: false,
            direct: false,
            dirty: false,
        }
    }

    /// A handle writing directly to the process's stderr (see FileHandle::direct).
    fn into_stderr_handle(self) -> FileHandle {
        let mut fh = self.into_handle();
        fh.direct = true;
        fh
    }
}

// Implementation of the "server" thread issuing the writes.

#[derive(Default)]
struct WriteBatch {
    io_vec: Vec<io::IoSlice<'static>>,
    requests: Vec<Request>,
    n_writes: usize,
    write_bytes: usize,
    flush: bool,
    close: bool,
    /// Whether `close` comes from an `Evict` request.
    evict: bool,
}

#[cfg(feature = "unstable")]
fn write_all(batch: &mut WriteBatch, w: &mut impl Write) -> io::Result</*close=*/ bool> {
    w.write_all_vectored(&mut batch.io_vec[..]).map(|_| true)
}

#[cfg(not(feature = "unstable"))]
fn write_all(batch: &mut WriteBatch, w: &mut impl Write) -> io::Result</*close=*/ bool> {
    batch
        .io_vec
        .iter()
        .map(|slice| {
            let buf: &[u8] = slice.deref();
            w.write_all(buf)
        })
        .fold_ok(true, |a, _| a)
}

impl WriteBatch {
    fn n_writes(&self) -> usize {
        self.n_writes
    }

    fn issue(&mut self, w: &mut impl Write) -> io::Result</*close=*/ bool> {
        if self.write_bytes > 0 {
            write_all(self, w)?;
        }
        if self.flush || self.close {
            w.flush()?;
        }
        let close = self.close;
        self.clear();
        Ok(close)
    }

    fn get_spec(&self) -> FileSpec {
        for req in self.requests.iter() {
            if let Request::Write { spec, .. } = req {
                return *spec;
            }
        }
        Default::default()
    }
    fn push(&mut self, req: Request) -> bool {
        match &req {
            Request::Write { data, flush, .. } => {
                // TODO: this does not handle payloads larger than 4GB on windows, see
                // documentation for IoSlice. Should be an easy fix if this comes up.
                let io_slice = io::IoSlice::new(unsafe { &**data });
                self.write_bytes += io_slice.len();
                self.io_vec.push(io_slice);
                self.n_writes += 1;
                self.flush |= *flush;
            }
            Request::Flush(_) => self.flush = true,
            Request::Close => self.close = true,
            Request::Evict(_) => {
                self.close = true;
                self.evict = true;
            }
        };
        self.requests.push(req);
        self.flush || self.close
    }
    fn clear_batch(&mut self, mut f: impl FnMut(&ErrorCode)) {
        self.io_vec.clear();
        for req in self.requests.drain(..) {
            req.set_code(&mut f)
        }
        self.close = false;
        self.evict = false;
        self.flush = false;
        self.n_writes = 0;
        self.write_bytes = 0;
    }
    fn clear_error(&mut self) {
        self.clear_batch(ErrorCode::set_error)
    }
    fn clear(&mut self) {
        self.clear_batch(ErrorCode::set_ok)
    }
}

/// The state a writer thread shares with the handles of its output.
struct WriterState<'a> {
    open: &'a AtomicBool,
    /// See `WriterSlot::evicted`.
    evicted: &'a AtomicBool,
    member: Option<&'a PoolMember>,
    slot: &'a Weak<WriterSlot>,
}

fn receive_thread<W: io::Write>(
    receiver: Receiver<Request>,
    error: Arc<Mutex<Option<CompileError>>>,
    state: WriterState,
    f: impl Fn(FileSpec) -> io::Result<W>,
    is_stdout: bool,
) {
    let mut batch = WriteBatch::default();
    if let Err(e) = receive_loop(&receiver, &mut batch, &state, f) {
        if is_stdout && e.kind() == io::ErrorKind::BrokenPipe {
            STDOUT_BROKEN_PIPE.store(true, Ordering::Release);
        }
        // The file is closed: no need to evict it.
        if let Some(m) = state.member {
            m.unregister();
        }
        // We got an error! install it in the `error` mutex.
        {
            let mut err = error.lock().unwrap();
            *err = Some(CompileError::new(format!("{}", e)));
        }
        // Now signal an error on any pending requests.
        batch.clear_error();
        // And send an error back for any more requests that come in.
        while let Ok(req) = receiver.recv() {
            req.set_code(ErrorCode::set_error)
        }
    }
}

fn receive_loop<W: io::Write>(
    receiver: &Receiver<Request>,
    batch: &mut WriteBatch,
    state: &WriterState,
    f: impl Fn(FileSpec) -> io::Result<W>,
) -> io::Result<()> {
    let WriterState {
        open,
        evicted,
        member,
        slot,
    } = *state;
    const MAX_BATCH_BYTES: usize = 1 << 20;
    const MAX_BATCH_SIZE: usize = 1 << 10;

    // Writer starts off closed. We use `f` to open it if a write appears; if it was closed by
    // an `Evict` request (see `evicted`), it reopens for appending.
    let mut writer = None;

    while let Ok(req) = receiver.recv() {
        // We build up a reasonably-sized batch of writes in the channel if it contains pending
        // operations in the channel.
        //
        // To simplify matters, we cut a batch short if we receive a "flush" or "close" request
        // (signaled by batch.push returning true).
        //
        // NB: this batching is redundant when writing to stdout (in which case grep_cli gives us a
        // buffered version of the writer), but we want it for the file IO case, where this
        // approach permits us fewer copies than the BufWriter approach.
        let mut batch_bytes = req.size();
        if !batch.push(req) {
            while let Ok(req) = receiver.try_recv() {
                batch_bytes += req.size();
                if batch.push(req)
                    || batch.n_writes() >= MAX_BATCH_SIZE
                    || batch_bytes >= MAX_BATCH_BYTES
                {
                    break;
                }
            }
        }
        if writer.is_none() {
            if batch.n_writes() == 0 {
                // check for a "flush/close-only batch", which we treat as a noop if the file is
                // closed.
                if batch.close && !batch.evict {
                    // An explicit close of an evicted file: it is truncated if reopened by `>`.
                    evicted.store(false, Ordering::Release);
                    open.store(false, Ordering::Release);
                }
                batch.clear();
                if WriterSlot::retire(slot, receiver) {
                    return Ok(());
                }
                continue;
            }
            // We need to (re)open the file, the first write request will tell us whether or not
            // this is an append request.
            let spec = if evicted.load(Ordering::Acquire) {
                FileSpec::Append
            } else {
                batch.get_spec()
            };
            writer = Some(f(spec)?);
            evicted.store(false, Ordering::Release);
            open.store(true, Ordering::Release);
            if let Some(m) = member {
                m.register();
            }
        } else if let Some(m) = member {
            m.touch();
        }
        let evict = batch.evict;
        if batch.issue(writer.as_mut().unwrap())? {
            writer = None;
            if let Some(m) = member {
                m.unregister();
            }
            if evict {
                evicted.store(true, Ordering::Release);
            } else {
                open.store(false, Ordering::Release);
            }
            // The output is closed: stop this thread until it is written to again.
            if WriterSlot::retire(slot, receiver) {
                return Ok(());
            }
        }
    }
    Ok(())
}

pub mod testing {
    use super::*;

    /// A file factory that writes all data in memory; used for unit testing.
    #[derive(Clone, Default)]
    pub struct FakeFs {
        pub stdout: FakeFile,
        named: Arc<Mutex<HashMap<String, FakeFile>>>,
    }

    impl FakeFs {
        pub fn get_handle(&self, path: &str) -> Option<FakeFile> {
            self.named.lock().unwrap().get(path).cloned()
        }
    }

    impl FileFactory for FakeFs {
        type Output = FakeFile;
        type Stdout = FakeFile;
        fn build(&self, path: &Path, spec: FileSpec) -> io::Result<Self::Output> {
            let path = path.to_string_lossy();
            let mut named = self.named.lock().unwrap();
            if let Some(file) = named.get(path.as_ref()) {
                file.reopen(spec);
                return Ok(file.clone());
            }
            let new_file = FakeFile::default();
            named.insert(path.into_owned(), new_file.clone());
            Ok(new_file)
        }
        fn stdout(&self) -> Self::Stdout {
            self.stdout.clone()
        }
    }

    #[derive(Default)]
    struct FakeFileInner {
        data: Mutex<Vec<u8>>,
        poison: AtomicBool,
    }

    impl FakeFileInner {
        fn result(&self) -> io::Result<()> {
            if self.poison.load(Ordering::Acquire) {
                Err(io::Error::new(io::ErrorKind::Other, "poisoned fake file!"))
            } else {
                Ok(())
            }
        }
    }

    /// The files stored in a FakeFs.
    ///
    /// These are primarily vectors of bytes, but they can also be configured to return errors or
    /// block write requests.
    #[derive(Clone, Default)]
    pub struct FakeFile(Arc<FakeFileInner>);

    impl FakeFile {
        pub fn set_poison(&self, p: bool) {
            self.0.poison.store(p, Ordering::Release);
        }
        pub fn read_data(&self) -> Vec<u8> {
            (*self.0.data.lock().unwrap()).clone()
        }
        pub fn reopen(&self, spec: FileSpec) {
            if !matches!(spec, FileSpec::Append) {
                self.clear();
            }
        }
        pub fn clear(&self) {
            self.0.data.lock().unwrap().clear();
        }
    }

    impl Write for FakeFile {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.result()?;
            self.0.data.lock().unwrap().extend(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.result()?;
            Ok(())
        }
        fn write_vectored(&mut self, bufs: &[io::IoSlice]) -> io::Result<usize> {
            self.0.result()?;
            let mut written = 0;
            let mut data = self.0.data.lock().unwrap();
            for b in bufs {
                let bytes: &[u8] = b;
                data.extend(bytes);
                written += bytes.len();
            }
            Ok(written)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn basic_writing() {
        let s1 = Str::from("hello");
        let s2 = Str::from(" there");
        let fs = FakeFs::default();
        let mut reg = Registry::from_factory(fs.clone());
        {
            let handle = reg
                .get_handle(/*stdout*/ None, FileSpec::default())
                .unwrap();
            handle.write(&s1, FileSpec::Append).unwrap();
            handle.write(&s2, FileSpec::Append).unwrap();
            handle.flush().unwrap();
            handle.write(&s1, FileSpec::Append).unwrap();
            handle.write(&s2, FileSpec::Append).unwrap();
            handle.flush().unwrap();
        }
        let data = fs.stdout.read_data();
        assert_eq!(&data[..], "hello therehello there".as_bytes());
    }

    #[test]
    fn reopen_named_file() {
        let fname_str = "/fake";
        let fname = Str::from(fname_str);
        let s1 = Str::from("hello");
        let s2 = Str::from(" there");
        let fs = FakeFs::default();
        let mut reg = Registry::from_factory(fs.clone());
        {
            let handle = reg.get_handle(Some(&fname), FileSpec::default()).unwrap();
            handle.write(&s1, FileSpec::Append).unwrap();
            handle.write(&s2, FileSpec::Append).unwrap();
            handle.flush().unwrap();
            handle.write(&s1, FileSpec::Append).unwrap();
            handle.write(&s2, FileSpec::Append).unwrap();
        }
        {
            let handle = reg.get_handle(Some(&fname), FileSpec::default()).unwrap();
            handle.close().unwrap();
            handle.write(&s1, FileSpec::Trunc).unwrap();
            handle.write(&s2, FileSpec::Trunc).unwrap();
            handle.flush().unwrap();
        }
        let data = fs.get_handle(fname_str).unwrap().read_data();
        assert_eq!(&data[..], "hello there".as_bytes());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_name() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(bytes_path(b"out\xff.txt").as_os_str().as_bytes(), b"out\xff.txt");
        let fname = Str::from_bytes(b"/fake/out\xff.txt");
        let fs = FakeFs::default();
        let mut reg = Registry::from_factory(fs.clone());
        reg.check_output(&fname, FileSpec::Trunc).unwrap();
        let handle = reg.get_handle(Some(&fname), FileSpec::Trunc).unwrap();
        handle.write(&Str::from("x"), FileSpec::Trunc).unwrap();
        handle.flush().unwrap();
        assert!(reg.flush_named(&fname).unwrap());
        assert_eq!(reg.close(&fname).unwrap(), Some(0));
        let data = fs.get_handle("/fake/out\u{FFFD}.txt").unwrap().read_data();
        assert_eq!(&data[..], b"x");
    }

    #[test]
    fn multithreaded_write() {
        const N_THREADS: usize = 100;
        const WRITES_PER_THREAD: usize = 1000;
        let fs = FakeFs::default();
        fs.build(Path::new("/fake/BAD"), FileSpec::Trunc)
            .unwrap()
            .set_poison(true);
        let mut threads = Vec::with_capacity(N_THREADS);
        {
            let reg = Registry::from_factory(fs.clone());
            for t in 0..N_THREADS {
                let mut treg = reg.clone();
                threads.push(std::thread::spawn(move || {
                    let a = Str::from("A");
                    let b = Str::from("B");
                    let fa = Str::from("/fake/A");
                    let fb = Str::from("/fake/B");
                    let fbad = Str::from("/fake/BAD");
                    for i in 0..WRITES_PER_THREAD {
                        {
                            let h1 = treg.get_handle(Some(&fa), FileSpec::default()).unwrap();
                            h1.write(&a, FileSpec::Append).unwrap();
                            if (t + i) % 100 == 0 {
                                h1.close().unwrap();
                            }
                        }
                        {
                            // We do not close file b, so append=false should not matter.
                            let h2 = treg.get_handle(Some(&fb), FileSpec::default()).unwrap();
                            h2.write(&b, FileSpec::Trunc).unwrap();
                            if (t + i) % 105 == 0 {
                                h2.flush().unwrap();
                            }
                        }

                        {
                            let h3 = treg.get_handle(Some(&fbad), FileSpec::default()).unwrap();
                            // These won't all be errors.
                            let _ = h3.write(&a, FileSpec::Append);
                            if (t + i) % 103 == 0 {
                                // But all of these will be
                                assert!(h3.flush().is_err())
                            }
                        }
                    }
                }));
            }
        }
        for t in threads.into_iter() {
            t.join().unwrap();
        }
        let expected_a = vec![b'A'; N_THREADS * WRITES_PER_THREAD];
        let expected_b = vec![b'B'; N_THREADS * WRITES_PER_THREAD];
        assert_eq!(fs.get_handle("/fake/A").unwrap().read_data(), expected_a);
        assert_eq!(fs.get_handle("/fake/B").unwrap().read_data(), expected_b);
    }
}
