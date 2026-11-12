use std::borrow::Borrow;
use std::io::Read;
use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

use crate::common::{CancelSignal, CompileError, Result};
use crate::runtime::{
    splitter::{
        Reader,
        batch::{
            BytesIndexKernel, CsvIndexKernel, InputFormat, MAX_CHUNK_LEN, Offsets,
            WhitespaceIndexKernel, WhitespaceOffsets, csv_record_end, find_record_seps,
            get_find_indexes,
        },
        pread::BlockRead,
    },
    str_impl::UniqueBuf,
};

thread_local! {
    /// The number of the chunk with records this thread took last from a `ParallelChunkProducer`
    /// (`-pr`), see [`take_last_chunk`].
    static LAST_CHUNK: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The number of the last chunk with records this thread took from a `ParallelChunkProducer`
/// since the previous call, if any. Chunks are numbered in input order, so the worker holding the
/// largest one read the last record of the input (see `interp::Core::keep_last_record`).
pub(crate) fn take_last_chunk() -> Option<u64> {
    LAST_CHUNK.with(std::cell::Cell::take)
}

/// A vector of dynamically typed factory functions producing a chunk producer
/// for a particular chunk type in a different thread.
type DynamicProducers<C> = Vec<Box<dyn FnOnce() -> Box<dyn ChunkProducer<Chunk = C>> + Send>>;

pub trait ChunkProducer {
    type Chunk: Chunk;
    // Create up to _requested_size additional handles to the ChunkProducer, if possible. Chunk is
    // Send, so these new producers can be used to read from the same source in parallel.
    //
    // NB: what's going on with this gnarly return type? All implementations either return an empty
    // vector or a vector containing functions returning Box<Self>, but we need this trait to be
    // object-safe, so it returns a trait object instead.
    //
    // Why return a FnOnce rather than  a trait object directly? Some ChunkProducer
    // implementations are not Send, even though the data to initialize a new ChunkProducer reading
    // from the same source is Send. Passing a FnOnce allows us to handle this, which is the case
    // for (e.g.) ShardedChunkProducer.
    fn try_dyn_resize(&self, _requested_size: usize) -> DynamicProducers<Self::Chunk> {
        vec![]
    }
    fn wait(&self) -> bool {
        true
    }
    // Like `wait`, but give up early once `signal` is cancelled (e.g. by `exit`).
    fn wait_cancellable(&self, _signal: &CancelSignal) -> bool {
        self.wait()
    }
    fn get_chunk(&mut self, chunk: &mut Self::Chunk) -> Result<bool /*done*/>;
    fn next_file(&mut self) -> Result<bool /*new file available*/>;
    // Leave the parts of a chunk that do not depend on the chunks before it to whoever consumes
    // it (see `Chunk::finish`), when the producer supports it. Called by `ParallelChunkProducer`
    // on the producer run by its reading thread, so that the workers do that work in parallel.
    fn defer_scan(&mut self) {}
    // Called once the input is cancelled (see `CancellableChunkProducer`): a producer fed by a
    // background thread tells it to stop, so that it releases its input although the consumers
    // still hold the producer (the main thread keeps it until the process exits).
    fn stop(&self) {}
}

// Send `item` on `sender` unless `stop` is set first. Returns false when the item was not sent,
// because of `stop` or because all receivers are gone, and the sending thread should end.
fn send_unless_stopped<T>(sender: &Sender<T>, mut item: T, stop: &AtomicBool) -> bool {
    use crossbeam_channel::SendTimeoutError;
    use std::time::Duration;
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        match sender.send_timeout(item, Duration::from_millis(20)) {
            Ok(()) => return true,
            Err(SendTimeoutError::Timeout(it)) => item = it,
            Err(SendTimeoutError::Disconnected(_)) => return false,
        }
    }
}

pub trait Chunk: Send + Default {
    fn get_name(&self) -> &str;
    // Do the work a producer left after `defer_scan` (or the read of a block, see
    // `pread::PreadChunkProducer`); called once on each chunk received. `prev` is the chunk
    // consumed before, whose buffer it may reuse. Returns false for a chunk without any record,
    // to be skipped.
    fn finish(&mut self, _prev: &mut Self) -> Result<bool> {
        Ok(true)
    }
}

/// The scan of a chunk left to the worker that takes it (see `ChunkProducer::defer_scan`). The
/// chunk ends at a record separator (or at the end of its file), so the scan does not depend on
/// the chunks before it, and gives the offsets the producer would have kept.
#[derive(Copy, Clone)]
pub enum DeferredScan {
    /// Only the record separators (see `Offsets::lines_only`): in `rel` for single-byte splitting,
    /// in `nl` for whitespace splitting.
    Lines { record_sep: u8 },
    Bytes {
        kernel: BytesIndexKernel,
        field_sep: u8,
        record_sep: u8,
    },
    /// The chunk starts after a newline, i.e. after whitespace. `trim` drops the offsets after
    /// the last byte, which the scan finds in the zero padding (zero is not whitespace), as the
    /// producer does for all chunks but the last one of a file.
    Whitespace {
        kernel: WhitespaceIndexKernel,
        trim: bool,
    },
    /// CSV or TSV. A CSV chunk ends at a newline outside quotes, found by [csv_record_end], so the
    /// scan starts outside quotes, as the producer's does at the start of a chunk.
    Csv {
        kernel: CsvIndexKernel,
        quoted: bool,
    },
}

/// The offsets a [DeferredScan] fills in.
pub trait ScanOffsets: Default + Send {
    fn clear_offsets(&mut self);
    fn set_lines_only(&mut self, lines_only: bool);
    fn scan(&mut self, buf: &[u8], scan: DeferredScan);
}

impl ScanOffsets for Offsets {
    fn clear_offsets(&mut self) {
        self.clear();
    }
    fn set_lines_only(&mut self, lines_only: bool) {
        self.lines_only = lines_only;
    }
    fn scan(&mut self, buf: &[u8], scan: DeferredScan) {
        match scan {
            DeferredScan::Lines { record_sep } => {
                self.clear();
                find_record_seps(buf, 0, &mut self.rel.fields, record_sep);
            }
            DeferredScan::Bytes {
                kernel,
                field_sep,
                record_sep,
            } => {
                unsafe { kernel(buf, self, field_sep, record_sep) };
                // As the producer does: no offset is past the end of the chunk.
                let keep = self
                    .rel
                    .fields
                    .partition_point(|&off| (off as usize) < buf.len());
                self.rel.fields.truncate(keep);
            }
            DeferredScan::Csv { kernel, .. } => {
                self.clear();
                unsafe { kernel(buf, self, 0, 0) };
                let keep = self
                    .rel
                    .fields
                    .partition_point(|&off| (off as usize) < buf.len());
                self.rel.fields.truncate(keep);
            }
            DeferredScan::Whitespace { .. } => {
                unreachable!("whitespace scan of single-byte offsets")
            }
        }
    }
}

impl ScanOffsets for WhitespaceOffsets {
    fn clear_offsets(&mut self) {
        self.0.clear();
    }
    fn set_lines_only(&mut self, lines_only: bool) {
        self.0.lines_only = lines_only;
    }
    fn scan(&mut self, buf: &[u8], scan: DeferredScan) {
        match scan {
            DeferredScan::Lines { record_sep } => {
                self.0.clear();
                find_record_seps(buf, 0, &mut self.0.nl.fields, record_sep);
            }
            DeferredScan::Whitespace { kernel, trim } => {
                unsafe {
                    kernel(buf, self, /*start: after whitespace*/ 1)
                };
                if trim {
                    let fields = &mut self.0.rel.fields;
                    let keep = fields.partition_point(|&off| (off as usize) < buf.len());
                    fields.truncate(keep);
                }
            }
            DeferredScan::Bytes { .. } | DeferredScan::Csv { .. } => {
                unreachable!("single-byte scan of whitespace offsets")
            }
        }
    }
}

#[derive(Copy, Clone)]
enum ChunkState {
    Init,
    Main,
    Done,
}

/// Set by the reader when the program uses no field and does not read NF: chunks produced while
/// it is set only hold the record boundaries (see `Offsets::lines_only`). It is checked once per
/// chunk, so it may change while chunks are being produced.
pub type LinesOnly = Arc<AtomicBool>;

pub struct OffsetChunkProducer<R, F> {
    inner: Reader<R>,
    cur_file_version: u32,
    name: Arc<str>,
    find_indexes: F,
    record_sep: u8,
    state: ChunkState,
    lines_only: Option<LinesOnly>,
    /// The scan the producer can leave to the consumers, and whether it does (see
    /// `ChunkProducer::defer_scan`).
    deferrable: Option<DeferredScan>,
    defer: bool,
}

impl<R: Read, F> OffsetChunkProducer<R, F> {
    fn lines_only(&self) -> bool {
        self.lines_only
            .as_ref()
            .is_some_and(|l| l.load(Ordering::Relaxed))
    }

    /// With `defer_scan`, yield the next chunk without scanning it: only find its last record
    /// separator, and leave the scan to `Chunk::finish`. Returns `None` when the scan is not
    /// deferred, the result of `get_chunk` otherwise.
    ///
    /// A CSV chunk can only be cut where the quotes before are known to be balanced (see
    /// [csv_record_end]); when they are not, this returns `None` as well, with the reader where it
    /// was, and the producer scans that chunk itself.
    fn get_deferred_chunk<Off: ScanOffsets>(
        &mut self,
        chunk: &mut OffsetChunk<Off>,
    ) -> Option<Result<bool>> {
        let scan = self.deferrable.filter(|_| self.defer)?;
        self.get_deferred_chunk_inner(chunk, scan)
    }

    fn get_deferred_chunk_inner<Off: ScanOffsets>(
        &mut self,
        chunk: &mut OffsetChunk<Off>,
        scan: DeferredScan,
    ) -> Option<Result<bool>> {
        macro_rules! tryr {
            ($e:expr) => {
                match $e {
                    Ok(x) => x,
                    Err(e) => return Some(Err(e)),
                }
            };
        }
        loop {
            match self.state {
                ChunkState::Init => {
                    self.state = if tryr!(self.inner.reset()) {
                        ChunkState::Done
                    } else {
                        ChunkState::Main
                    };
                }
                ChunkState::Main => {
                    tryr!(check_chunk_len(self.inner.end));
                    let buf = self.inner.buf.clone();
                    let end = self.inner.end;
                    let bs = &buf.as_bytes()[..end];
                    // A buffer without a record separator is read again with more data after
                    // it; memrchr over it again is cheap next to the read.
                    let target = match scan {
                        DeferredScan::Csv { quoted: true, .. } => {
                            // Unbalanced quotes before every newline (dirty data, or a quoted
                            // field longer than the buffer): scan this chunk here. So too when
                            // the cut is in the first half, typically before an irregular quote:
                            // cutting there would make small chunks out of dirty data, each
                            // followed by one this producer scans anyway.
                            let t = csv_record_end(bs)?;
                            if t < end / 2 {
                                return None;
                            }
                            Some(t)
                        }
                        _ => memchr::memrchr(self.record_sep, bs).map(|i| i + 1),
                    };
                    if let Some(t) = target {
                        self.inner.start = t;
                    }
                    // See the comments on chunk.len in `OffsetChunkProducer::get_chunk`.
                    let is_eof = tryr!(self.inner.reset());
                    let mut scan = scan;
                    let len = match (target, is_eof) {
                        (Some(t), false) => t,
                        (None, true) if end == 0 => {
                            self.inner.clear_buf();
                            self.state = ChunkState::Done;
                            continue;
                        }
                        (_, true) => {
                            self.inner.clear_buf();
                            self.state = ChunkState::Done;
                            // The producer keeps every offset of the last chunk of a file.
                            if let DeferredScan::Whitespace { trim, .. } = &mut scan {
                                *trim = false;
                            }
                            end
                        }
                        (None, false) => continue,
                    };
                    let lines_only = self.lines_only();
                    chunk.version = self.cur_file_version;
                    chunk.name = self.name.clone();
                    chunk.buf = Some(buf.try_unique().unwrap());
                    chunk.len = len;
                    chunk.off.clear_offsets();
                    chunk.off.set_lines_only(lines_only);
                    chunk.deferred = Some(if lines_only {
                        DeferredScan::Lines {
                            record_sep: self.record_sep,
                        }
                    } else {
                        scan
                    });
                    return Some(Ok(false));
                }
                ChunkState::Done => return Some(Ok(true)),
            }
        }
    }
}

/// Offsets are stored as `u32` (see `OffsetInner::fields`): a record cannot be longer than that.
pub(super) fn check_chunk_len(len: usize) -> Result<()> {
    if len > MAX_CHUNK_LEN {
        return err!(
            "record too long: more than {} bytes without a record separator",
            MAX_CHUNK_LEN
        );
    }
    Ok(())
}

/// A scan of a buffer without a complete record, to be continued on the next (longer) buffer,
/// which starts with the same bytes.
#[derive(Copy, Clone)]
struct PartialScan {
    lines_only: bool,
    /// The bytes scanned.
    end: usize,
    /// The number of offsets in `rel` that are known not to end a record.
    rel: usize,
}

pub fn new_offset_chunk_producer_csv<R: Read>(
    r: R,
    chunk_size: usize,
    name: &str,
    ifmt: InputFormat,
    start_version: u32,
    check_utf8: bool,
) -> OffsetChunkProducer<R, impl FnMut(&[u8], &mut Offsets) + use<R>> {
    let find_indexes = get_find_indexes(ifmt);
    OffsetChunkProducer {
        name: name.into(),
        inner: Reader::new(r, chunk_size, /*padding=*/ 128, check_utf8),
        find_indexes: move |bs: &[u8], offs: &mut Offsets| {
            unsafe { find_indexes(bs, offs, 0, 0) };
        },
        record_sep: b'\n',
        cur_file_version: start_version,
        state: ChunkState::Init,
        lines_only: None,
        deferrable: Some(DeferredScan::Csv {
            kernel: find_indexes,
            quoted: ifmt == InputFormat::CSV,
        }),
        defer: false,
    }
}

pub fn new_offset_chunk_producer_bytes<R: Read>(
    r: R,
    chunk_size: usize,
    name: &str,
    field_sep: u8,
    record_sep: u8,
    start_version: u32,
    check_utf8: bool,
    find_indexes: BytesIndexKernel,
    lines_only: LinesOnly,
) -> OffsetChunkProducer<R, impl FnMut(&[u8], &mut Offsets) + use<R>> {
    OffsetChunkProducer {
        name: name.into(),
        inner: Reader::new(r, chunk_size, /*padding=*/ 128, check_utf8),
        find_indexes: move |bs: &[u8], offs: &mut Offsets| unsafe {
            find_indexes(bs, offs, field_sep, record_sep)
        },
        cur_file_version: start_version,
        record_sep,
        state: ChunkState::Init,
        lines_only: Some(lines_only),
        deferrable: Some(DeferredScan::Bytes {
            kernel: find_indexes,
            field_sep,
            record_sep,
        }),
        defer: false,
    }
}

pub fn new_offset_chunk_producer_ascii_whitespace<R: Read>(
    r: R,
    chunk_size: usize,
    name: &str,
    start_version: u32,
    check_utf8: bool,
    find_indexes: WhitespaceIndexKernel,
    lines_only: LinesOnly,
) -> WhitespaceChunkProducer<R, impl FnMut(&[u8], &mut WhitespaceOffsets, u64) -> u64 + use<R>> {
    WhitespaceChunkProducer(
        OffsetChunkProducer {
            name: name.into(),
            inner: Reader::new(r, chunk_size, /*padding=*/ 128, check_utf8),
            find_indexes: move |bs: &[u8], offs: &mut WhitespaceOffsets, start: u64| unsafe {
                find_indexes(bs, offs, start)
            },
            cur_file_version: start_version,
            record_sep: b'\n',
            state: ChunkState::Init,
            lines_only: Some(lines_only),
            deferrable: Some(DeferredScan::Whitespace {
                kernel: find_indexes,
                trim: true,
            }),
            defer: false,
        },
        1,
    )
}

pub fn new_chained_offset_chunk_producer_csv<
    R: Read,
    N: Borrow<str>,
    I: Iterator<Item = (R, N)>,
>(
    r: I,
    chunk_size: usize,
    ifmt: InputFormat,
    check_utf8: bool,
) -> ChainedChunkProducer<OffsetChunkProducer<R, impl FnMut(&[u8], &mut Offsets)>> {
    ChainedChunkProducer::new(
        r.enumerate()
            .map(|(i, (r, name))| {
                new_offset_chunk_producer_csv(
                    r,
                    chunk_size,
                    name.borrow(),
                    ifmt,
                    /*start_version=*/ (i as u32).wrapping_add(1),
                    check_utf8,
                )
            })
            .collect(),
    )
}

pub fn new_chained_offset_chunk_producer_bytes<
    R: Read,
    N: Borrow<str>,
    I: Iterator<Item = (R, N)>,
>(
    r: I,
    chunk_size: usize,
    field_sep: u8,
    record_sep: u8,
    check_utf8: bool,
    kernel: BytesIndexKernel,
    lines_only: LinesOnly,
) -> ChainedChunkProducer<OffsetChunkProducer<R, impl FnMut(&[u8], &mut Offsets)>> {
    ChainedChunkProducer::new(
        r.enumerate()
            .map(|(i, (r, name))| {
                new_offset_chunk_producer_bytes(
                    r,
                    chunk_size,
                    name.borrow(),
                    field_sep,
                    record_sep,
                    /*start_version=*/ (i as u32).wrapping_add(1),
                    check_utf8,
                    kernel,
                    lines_only.clone(),
                )
            })
            .collect(),
    )
}

pub fn new_chained_offset_chunk_producer_ascii_whitespace<
    R: Read,
    N: Borrow<str>,
    I: Iterator<Item = (R, N)>,
>(
    r: I,
    chunk_size: usize,
    check_utf8: bool,
    find_indexes: WhitespaceIndexKernel,
    lines_only: LinesOnly,
) -> ChainedChunkProducer<
    WhitespaceChunkProducer<R, impl FnMut(&[u8], &mut WhitespaceOffsets, u64) -> u64>,
> {
    ChainedChunkProducer::new(
        r.enumerate()
            .map(|(i, (r, name))| {
                new_offset_chunk_producer_ascii_whitespace(
                    r,
                    chunk_size,
                    name.borrow(),
                    /*start_version=*/ (i as u32).wrapping_add(1),
                    check_utf8,
                    find_indexes,
                    lines_only.clone(),
                )
            })
            .collect(),
    )
}

impl<C: Chunk> ChunkProducer for Box<dyn ChunkProducer<Chunk = C>> {
    type Chunk = C;
    fn try_dyn_resize(&self, requested_size: usize) -> DynamicProducers<C> {
        (**self).try_dyn_resize(requested_size)
    }
    fn wait(&self) -> bool {
        (**self).wait()
    }
    fn wait_cancellable(&self, signal: &CancelSignal) -> bool {
        (**self).wait_cancellable(signal)
    }
    fn next_file(&mut self) -> Result<bool> {
        (**self).next_file()
    }
    fn get_chunk(&mut self, chunk: &mut C) -> Result<bool> {
        (**self).get_chunk(chunk)
    }
    fn defer_scan(&mut self) {
        (**self).defer_scan()
    }
    fn stop(&self) {
        (**self).stop()
    }
}

pub struct OffsetChunk<Off = Offsets> {
    pub version: u32,
    pub name: Arc<str>,
    pub buf: Option<UniqueBuf>,
    pub len: usize,
    pub off: Off,
    /// The scan of `buf[..len]` that fills `off`, when the producer left it (see
    /// `ChunkProducer::defer_scan`).
    pub deferred: Option<DeferredScan>,
    /// The block of a file to read into `buf` before the scan, when the producer left that too
    /// (see `pread::PreadChunkProducer`).
    pub block: Option<BlockRead>,
}

impl<Off: Default> Default for OffsetChunk<Off> {
    fn default() -> OffsetChunk<Off> {
        OffsetChunk {
            version: 0,
            name: "".into(),
            buf: None,
            len: 0,
            off: Default::default(),
            deferred: None,
            block: None,
        }
    }
}

impl<Off: ScanOffsets> Chunk for OffsetChunk<Off> {
    fn get_name(&self) -> &str {
        &self.name
    }
    fn finish(&mut self, prev: &mut Self) -> Result<bool> {
        if let Some(block) = self.block.take() {
            // Read into the buffer this chunk came back with from an earlier worker, or else
            // into the one of the chunk consumed before.
            let Some(start) = block.first_record()? else {
                return Ok(false);
            };
            let spare = self.buf.take().or_else(|| prev.buf.take());
            let data = block.read(start, spare)?;
            let (scan, lines_only) = block.scan(data.at_eof);
            self.buf = Some(data.buf);
            self.len = data.len;
            self.off.clear_offsets();
            self.off.set_lines_only(lines_only);
            self.deferred = Some(scan);
        }
        if let Some(scan) = self.deferred.take() {
            let bs = match self.buf.as_ref() {
                Some(b) => &b.as_bytes()[..self.len],
                None => &[],
            };
            self.off.scan(bs, scan);
        }
        Ok(true)
    }
}

impl<R: Read, F: FnMut(&[u8], &mut Offsets)> ChunkProducer for OffsetChunkProducer<R, F> {
    type Chunk = OffsetChunk;
    fn next_file(&mut self) -> Result<bool> {
        self.state = ChunkState::Done;
        self.inner.force_eof();
        Ok(false)
    }
    fn defer_scan(&mut self) {
        self.defer = true;
    }
    fn get_chunk(&mut self, chunk: &mut OffsetChunk) -> Result<bool> {
        // The buffer of the consumed chunk is overwritten below: read the next chunk into it.
        self.inner.recycle(chunk.buf.take());
        if let Some(res) = self.get_deferred_chunk(chunk) {
            return res;
        }
        let mut pending: Option<PartialScan> = None;
        loop {
            match self.state {
                ChunkState::Init => {
                    self.state = if self.inner.reset()? {
                        ChunkState::Done
                    } else {
                        ChunkState::Main
                    };
                }
                ChunkState::Main => {
                    chunk.version = self.cur_file_version;
                    chunk.name = self.name.clone();
                    let buf = self.inner.buf.clone();
                    let bs = buf.as_bytes();
                    check_chunk_len(self.inner.end)?;
                    // A buffer without a complete record is read again with more data after it:
                    // only scan the new bytes (the ones before are unchanged).
                    let lines_only = self.lines_only();
                    let partial = pending.take().filter(|p| p.lines_only == lines_only);
                    chunk.off.lines_only = lines_only;
                    // Only scan the bytes read: the CSV kernel needs to know where the input ends.
                    if lines_only {
                        let from = match partial {
                            Some(p) => p.end,
                            None => {
                                chunk.off.clear();
                                0
                            }
                        };
                        find_record_seps(
                            &bs[..self.inner.end],
                            from,
                            &mut chunk.off.rel.fields,
                            self.record_sep,
                        );
                    } else {
                        if partial.is_some() {
                            chunk.off.continue_scan();
                        }
                        (self.find_indexes)(&bs[..self.inner.end], &mut chunk.off);
                    }
                    // The offsets kept from the previous scan do not end a record.
                    let known = partial.map_or(0, |p| p.rel).min(chunk.off.rel.fields.len());
                    let mut target = None;
                    let mut new_len = 0;
                    let mut always_truncate = chunk.off.rel.fields.len();
                    for (i, offset) in chunk.off.rel.fields.iter().enumerate().skip(known).rev() {
                        let offset = *offset as usize;
                        if offset >= self.inner.end {
                            always_truncate = i;
                            continue;
                        }
                        if bs[offset] == self.record_sep {
                            target = Some(offset + 1);
                            new_len = i + 1;
                            break;
                        }
                    }
                    debug_assert!(new_len <= always_truncate);
                    let is_partial = if let Some(chunk_end) = target {
                        self.inner.start = chunk_end;
                        false
                    } else {
                        true
                    };
                    let scanned = PartialScan {
                        lines_only,
                        end: self.inner.end,
                        rel: always_truncate,
                    };
                    // chunk.len is a bit tricky. There are two signals that we have to take into
                    // account:
                    // 1) Is this the last buffer? (is_eof)
                    // 2) Does this buffer contain a record separator? (!is_partial)
                    //
                    // If it is the last buffer then we should set chunk.len to self.inner.end,
                    // which is all of the bytes that were returned by the underlying reader. This
                    // is true regardless of whether or not there's a record separator in the
                    // input.
                    //
                    // If this is _not_ the last buffer and we do have a record separator, we need
                    // to adjust the length of the chunk to only encompass the buffer's contents up
                    // through the last record separator (target.unwrap()).
                    //
                    // Lastly, if it is not the last buffer and we do not have a record separator,
                    // we simply repeat this entire loop.
                    chunk.len = self.inner.end;
                    let is_eof = self.inner.reset()?;
                    return match (is_partial, is_eof) {
                        (false, false) => {
                            // Yield buffer, stay in main.
                            chunk.buf = Some(buf.try_unique().unwrap());
                            chunk.off.rel.fields.truncate(new_len);
                            chunk.len = target.unwrap();
                            Ok(false)
                        }
                        // Nothing is left (an empty file, or input ending at the end of the
                        // previous buffer): finish without yielding an empty chunk, which reads
                        // as the end of all input rather than of this file.
                        (true, true) if chunk.len == 0 => {
                            self.inner.clear_buf();
                            self.state = ChunkState::Done;
                            continue;
                        }
                        (false, true) | (true, true) => {
                            // Yield the entire buffer, this was the last piece of data.
                            self.inner.clear_buf();
                            chunk.buf = Some(buf.try_unique().unwrap());
                            chunk.off.rel.fields.truncate(always_truncate);
                            self.state = ChunkState::Done;
                            Ok(false)
                        }
                        // We read an entire chunk, but we didn't find a full record. Try again
                        // (note that the call to reset read in a larger chunk and would have kept
                        // a prefix), continuing this scan.
                        (true, false) => {
                            pending = Some(scanned);
                            continue;
                        }
                    };
                }
                ChunkState::Done => return Ok(true),
            }
        }
    }
}

pub struct WhitespaceChunkProducer<R, F>(OffsetChunkProducer<R, F>, u64);

impl<R: Read, F: FnMut(&[u8], &mut WhitespaceOffsets, u64) -> u64> ChunkProducer
    for WhitespaceChunkProducer<R, F>
{
    type Chunk = OffsetChunk<WhitespaceOffsets>;
    fn next_file(&mut self) -> Result<bool> {
        self.0.state = ChunkState::Done;
        self.0.inner.force_eof();
        Ok(false)
    }
    fn defer_scan(&mut self) {
        self.0.defer = true;
    }
    fn get_chunk(&mut self, chunk: &mut Self::Chunk) -> Result<bool> {
        self.0.inner.recycle(chunk.buf.take());
        if let Some(res) = self.0.get_deferred_chunk(chunk) {
            return res;
        }
        let mut pending: Option<PartialScan> = None;
        loop {
            match self.0.state {
                ChunkState::Init => {
                    self.0.state = if self.0.inner.reset()? {
                        ChunkState::Done
                    } else {
                        ChunkState::Main
                    };
                }
                ChunkState::Main => {
                    chunk.version = self.0.cur_file_version;
                    chunk.name = self.0.name.clone();
                    let buf = self.0.inner.buf.clone();
                    let bs = buf.as_bytes();
                    // Whether the byte before this buffer is whitespace. The scan below updates
                    // `self.1` to the state at the end of the buffer.
                    let start_ws = self.1;
                    check_chunk_len(self.0.inner.end)?;
                    // See OffsetChunkProducer::get_chunk: continue the scan of a buffer without
                    // a complete record.
                    let lines_only = self.0.lines_only();
                    let partial = pending.take().filter(|p| p.lines_only == lines_only);
                    chunk.off.0.lines_only = lines_only;
                    if lines_only {
                        // The records end at newlines; the fields are split when they are used.
                        let from = match partial {
                            Some(p) => p.end,
                            None => {
                                chunk.off.0.clear();
                                0
                            }
                        };
                        find_record_seps(
                            &bs[..self.0.inner.end],
                            from,
                            &mut chunk.off.0.nl.fields,
                            b'\n',
                        );
                        self.1 = 1;
                    } else {
                        if partial.is_some() {
                            // The saved state is used instead of start_ws.
                            chunk.off.0.continue_scan();
                        }
                        // Only scan the bytes read, so that the scan can be continued on a
                        // longer buffer; the zero padding after them is not whitespace either.
                        self.1 = (self.0.find_indexes)(
                            &bs[..self.0.inner.end],
                            &mut chunk.off,
                            start_ws,
                        );
                    }
                    let scanned = PartialScan {
                        lines_only,
                        end: self.0.inner.end,
                        rel: 0,
                    };
                    // Find the last newline in the buffer, if there is one.
                    let (is_partial, truncate_to, len_if_not_last) =
                        if let Some(nl_off) = chunk.off.0.nl.fields.last().cloned() {
                            let buf_end = nl_off as usize + 1;
                            self.0.inner.start = buf_end;
                            // Keep the field offsets up to the last newline; the ones after it
                            // belong to the records of the next buffer. A field ends at the latest
                            // at the newline, so these are complete start/end pairs, the end of
                            // the last field included (e.g. the tab in "x\t\n").
                            let fields = &chunk.off.0.rel.fields;
                            let keep = fields
                                .iter()
                                .rposition(|&off| off <= nl_off)
                                .map_or(0, |i| i + 1);
                            (false, keep, buf_end)
                        } else {
                            (true, 0, 0)
                        };
                    // See comments in get_chunk for OffsetChunkProducer<R, F>
                    chunk.len = self.0.inner.end;
                    let is_eof = self.0.inner.reset()?;
                    return match (is_partial, is_eof) {
                        (false, false) => {
                            // Yield buffer, stay in main. The next buffer starts right after a
                            // newline, which is whitespace.
                            self.1 = 1;
                            chunk.buf = Some(buf.try_unique().unwrap());
                            chunk.off.0.rel.fields.truncate(truncate_to);
                            chunk.len = len_if_not_last;
                            Ok(false)
                        }
                        // See the comment in OffsetChunkProducer::get_chunk.
                        (true, true) if chunk.len == 0 => {
                            self.0.inner.clear_buf();
                            self.0.state = ChunkState::Done;
                            continue;
                        }
                        (false, true) | (true, true) => {
                            // Yield the entire buffer, this was the last piece of data.
                            self.0.inner.clear_buf();
                            chunk.buf = Some(buf.try_unique().unwrap());
                            self.0.state = ChunkState::Done;
                            Ok(false)
                        }
                        // We read an entire chunk, but we didn't find a full record. Try again
                        // (note that the call to reset read in a larger chunk and would have kept
                        // a prefix). The new buffer is scanned from the same start, so it starts
                        // from the same whitespace state, not the one at the end of this buffer.
                        (true, false) => {
                            self.1 = start_ws;
                            pending = Some(scanned);
                            continue;
                        }
                    };
                }
                ChunkState::Done => return Ok(true),
            }
        }
    }
}

pub struct ChainedChunkProducer<P>(Vec<P>);

impl<P> ChainedChunkProducer<P> {
    fn new(mut v: Vec<P>) -> ChainedChunkProducer<P> {
        v.reverse();
        ChainedChunkProducer(v)
    }
}

impl<P: ChunkProducer> ChunkProducer for ChainedChunkProducer<P> {
    type Chunk = P::Chunk;

    fn wait(&self) -> bool {
        
        if let Some(cur) = self.0.last() {
            cur.wait()
        } else {
            true
        }
    }

    fn next_file(&mut self) -> Result<bool> {
        if let Some(cur) = self.0.last_mut() {
            if !cur.next_file()? {
                let _last = self.0.pop();
                debug_assert!(_last.is_some());
            }
            Ok(!self.0.is_empty())
        } else {
            Ok(false)
        }
    }

    fn defer_scan(&mut self) {
        for p in self.0.iter_mut() {
            p.defer_scan();
        }
    }

    fn get_chunk(&mut self, chunk: &mut P::Chunk) -> Result<bool> {
        while let Some(cur) = self.0.last_mut() {
            if !cur.get_chunk(chunk)? {
                return Ok(false);
            }
            let _last = self.0.pop();
            debug_assert!(_last.is_some());
        }
        Ok(true)
    }
}

/// ParallelChunkProducer allows for consumption of individual chunks from a ChunkProducer in
/// parallel.
pub struct ParallelChunkProducer<P: ChunkProducer> {
    start: Receiver<()>,
    /// The chunks, numbered in the order of the input (see `writers::ordered`).
    incoming: Receiver<(u64, P::Chunk)>,
    spent: Sender<P::Chunk>,
    // The error that stopped the reading thread, set before it closes `incoming`.
    error: Arc<Mutex<Option<CompileError>>>,
    // Set by `stop`: the reading thread ends, dropping its producer (and so its input).
    stop: Arc<AtomicBool>,
}

impl<P: ChunkProducer> Clone for ParallelChunkProducer<P> {
    fn clone(&self) -> ParallelChunkProducer<P> {
        ParallelChunkProducer {
            start: self.start.clone(),
            incoming: self.incoming.clone(),
            spent: self.spent.clone(),
            error: self.error.clone(),
            stop: self.stop.clone(),
        }
    }
}

impl<P: ChunkProducer + 'static> ParallelChunkProducer<P> {
    pub fn new(
        p_factory: impl FnOnce() -> P + Send + 'static,
        chan_size: usize,
    ) -> ParallelChunkProducer<P> {
        let (start_sender, start_receiver) = bounded(chan_size);
        let (in_sender, in_receiver) = bounded(chan_size);
        let (spent_sender, spent_receiver) = bounded(chan_size);
        let error = Arc::new(Mutex::new(None));
        let reader_error = error.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let reader_stop = stop.clone();
        std::thread::spawn(move || {
            let mut p = p_factory();
            // The workers scan the chunks, so that this thread only reads them.
            p.defer_scan();
            let mut seq = 0u64;
            loop {
                if reader_stop.load(Ordering::Acquire) {
                    return;
                }
                let mut chunk = spent_receiver.try_recv().ok().unwrap_or_default();
                match p.get_chunk(&mut chunk) {
                    Ok(false) => {}
                    Ok(true) => return,
                    Err(e) => {
                        *reader_error.lock().unwrap_or_else(PoisonError::into_inner) = Some(e);
                        return;
                    }
                }
                let item = (seq, chunk);
                seq += 1;
                let item = match in_sender.try_send(item) {
                    Ok(()) => continue,
                    Err(TrySendError::Full(item)) => item,
                    Err(TrySendError::Disconnected(_)) => return,
                };
                // The channel is full: the running workers consume chunks slower than we produce
                // them, so start one more. Workers start one at a time, each time the channel
                // fills up again, so an IO-bound input (the channel never fills) keeps running on
                // one worker, while a CPU-bound one reaches all of them after `-j` full channels.
                // A worker that is not started waits until the input ends (see `wait`), it does
                // not give up after a timeout. A token sent while no worker waits stays in the
                // channel and starts the next one that asks; `try_send` fails once the channel
                // holds `chan_size` unused tokens, which is more than enough.
                let _ = start_sender.try_send(());
                // Once stopped, nobody may receive any more: do not block forever on a full
                // channel, which the consumers still hold open.
                if !send_unless_stopped(&in_sender, item, &reader_stop) {
                    return;
                }
            }
        });
        ParallelChunkProducer {
            start: start_receiver,
            incoming: in_receiver,
            spent: spent_sender,
            error,
            stop,
        }
    }
}

impl<P: ChunkProducer + 'static> ChunkProducer for ParallelChunkProducer<P> {
    type Chunk = P::Chunk;
    fn try_dyn_resize(&self, requested_size: usize) -> DynamicProducers<P::Chunk> {
        let mut res = Vec::with_capacity(requested_size);
        for _ in 0..requested_size {
            let p = self.clone();
            res.push(Box::new(move || Box::new(p) as Box<dyn ChunkProducer<Chunk = P::Chunk>>) as _)
        }
        res
    }
    fn next_file(&mut self) -> Result<bool> {
        err!("nextfile is not supported in record-oriented parallel mode")
    }
    // Blocks until the reading thread asks for one more worker (true), or until it stops, at the
    // end of the input or on an error, and drops the sender (false).
    fn wait(&self) -> bool {
        self.start.recv().is_ok()
    }
    fn wait_cancellable(&self, signal: &CancelSignal) -> bool {
        use crossbeam_channel::RecvTimeoutError;
        use std::time::Duration;
        while !signal.cancelled() {
            match self.start.recv_timeout(Duration::from_millis(20)) {
                Ok(()) => return true,
                Err(RecvTimeoutError::Disconnected) => return false,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        false
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        // Free the chunks already read; the reading thread sees `stop` before sending another.
        while self.incoming.try_recv().is_ok() {}
    }
    fn get_chunk(&mut self, chunk: &mut P::Chunk) -> Result<bool> {
        while let Ok((seq, mut new_chunk)) = self.incoming.recv() {
            // The records of the previous chunk are all processed: with `--keep-order`, its
            // output can be written once the output of the chunks before it is.
            crate::runtime::writers::ordered::begin_chunk(seq);
            mem::swap(chunk, &mut new_chunk);
            let res = chunk.finish(&mut new_chunk);
            let _ = self.spent.try_send(new_chunk);
            match res {
                Ok(true) => {
                    LAST_CHUNK.with(|c| c.set(Some(seq)));
                    return Ok(false);
                }
                // No record starts in the block: take the next chunk.
                Ok(false) => continue,
                Err(e) => return Err(e),
            }
        }
        // All chunks are consumed. One consumer takes the error; the others just stop.
        match self
            .error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            Some(e) => Err(e),
            None => Ok(true),
        }
    }
}

enum ProducerState<T> {
    Init,
    Main(T),
    Done,
}

/// ShardedChunkProducer allows consuption of entire chunk producers in parallel
pub struct ShardedChunkProducer<P> {
    incoming: Receiver<Box<dyn FnOnce() -> P + Send>>,
    state: ProducerState<P>,
    // Set by `stop`: the feeding thread ends, dropping the inputs not read yet.
    stop: Arc<AtomicBool>,
}

impl<P: ChunkProducer + 'static> ShardedChunkProducer<P> {
    pub fn new<Iter>(ps: Iter) -> ShardedChunkProducer<P>
    where
        Iter: Iterator + 'static + Send,
        Iter::Item: FnOnce() -> P + 'static + Send,
    {
        // These are usually individual files, which should be fairly large, so we hard-code a
        // small buffer.
        let (sender, receiver) = bounded(1);
        let stop = Arc::new(AtomicBool::new(false));
        let feeder_stop = stop.clone();
        std::thread::spawn(move || {
            for p_factory in ps {
                let to_send: Box<dyn FnOnce() -> P + Send> = Box::new(p_factory);
                if !send_unless_stopped(&sender, to_send, &feeder_stop) {
                    return;
                }
            }
        });
        ShardedChunkProducer {
            incoming: receiver,
            state: ProducerState::Init,
            stop,
        }
    }

    fn refresh_producer(&mut self) -> bool {
        let next = if let Ok(p) = self.incoming.recv() {
            p
        } else {
            self.state = ProducerState::Done;
            return false;
        };
        self.state = ProducerState::Main(next());
        true
    }
}

impl<P: ChunkProducer + 'static> ChunkProducer for ShardedChunkProducer<P> {
    type Chunk = P::Chunk;
    fn try_dyn_resize(&self, requested_size: usize) -> DynamicProducers<P::Chunk> {
        let mut res = Vec::with_capacity(requested_size);
        for _ in 0..requested_size {
            let incoming = self.incoming.clone();
            let stop = self.stop.clone();
            res.push(Box::new(move || {
                Box::new(ShardedChunkProducer {
                    incoming,
                    state: ProducerState::Init,
                    stop,
                }) as Box<dyn ChunkProducer<Chunk = P::Chunk>>
            }) as _)
        }
        res
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        while self.incoming.try_recv().is_ok() {}
        if let ProducerState::Main(p) = &self.state {
            p.stop();
        }
    }
    fn next_file(&mut self) -> Result<bool> {
        match &mut self.state {
            ProducerState::Init => Ok(self.refresh_producer()),
            ProducerState::Done => Ok(false),
            ProducerState::Main(p) => Ok(p.next_file()? || self.refresh_producer()),
        }
    }
    fn get_chunk(&mut self, chunk: &mut Self::Chunk) -> Result<bool> {
        loop {
            match &mut self.state {
                ProducerState::Main(p) => {
                    if !p.get_chunk(chunk)? {
                        return Ok(false);
                    }
                    self.refresh_producer()
                }
                ProducerState::Init => self.refresh_producer(),
                ProducerState::Done => return Ok(true),
            };
        }
    }
}

/// A ChunkProducer that stops input early if a [CancelSignal] is triggered.
pub struct CancellableChunkProducer<P> {
    signal: CancelSignal,
    prod: P,
}

impl<P: ChunkProducer + 'static> CancellableChunkProducer<P> {
    pub fn new(signal: CancelSignal, prod: P) -> Self {
        Self { signal, prod }
    }
}

impl<P: ChunkProducer + 'static> ChunkProducer for CancellableChunkProducer<P> {
    type Chunk = P::Chunk;

    // TODO this API means we have two layers of virtual dispatch, which isn't great.
    // It's not the end of the world, but we should think about whether there is a way around this.
    fn try_dyn_resize(&self, requested_size: usize) -> DynamicProducers<P::Chunk> {
        let children = self.prod.try_dyn_resize(requested_size);
        let mut res = Vec::with_capacity(children.len());
        for factory in children.into_iter() {
            let signal = self.signal.clone();
            res.push(Box::new(move || {
                Box::new(CancellableChunkProducer {
                    signal,
                    prod: factory(),
                }) as Box<dyn ChunkProducer<Chunk = P::Chunk>>
            }) as _)
        }
        res
    }

    fn wait(&self) -> bool {
        self.prod.wait_cancellable(&self.signal)
    }

    fn stop(&self) {
        self.prod.stop()
    }
    fn next_file(&mut self) -> Result<bool> {
        if self.signal.cancelled() {
            self.prod.stop();
            return Ok(false);
        }
        self.prod.next_file()
    }
    fn get_chunk(&mut self, chunk: &mut P::Chunk) -> Result<bool> {
        if self.signal.cancelled() {
            // The input ends here for every consumer: let a background reader release it.
            self.prod.stop();
            return Ok(true);
        }
        self.prod.get_chunk(chunk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Basic machinery to turn an iterator into a ChunkProducer. This makes it easier to unit test
    // the "derived ChunkProducers" like ParallelChunkProducer.

    fn new_iter(
        low: usize,
        high: usize,
        name: &str,
    ) -> impl FnOnce() -> IterChunkProducer<std::ops::Range<usize>> + use<> {
        let name: Arc<str> = name.into();
        move || IterChunkProducer {
            iter: (low..high),
            name,
        }
    }

    struct IterChunkProducer<I> {
        iter: I,
        name: Arc<str>,
    }

    struct ItemChunk<T> {
        item: T,
        name: Arc<str>,
    }

    impl<T: Default> Default for ItemChunk<T> {
        fn default() -> ItemChunk<T> {
            ItemChunk {
                item: Default::default(),
                name: "".into(),
            }
        }
    }

    impl<T: Send + Default> Chunk for ItemChunk<T> {
        fn get_name(&self) -> &str {
            &self.name
        }
    }

    impl<I: Iterator> ChunkProducer for IterChunkProducer<I>
    where
        I::Item: Send + Default,
    {
        type Chunk = ItemChunk<I::Item>;
        fn next_file(&mut self) -> Result<bool> {
            // clear remaining items
            for _ in self.iter.by_ref() {}
            Ok(false)
        }
        fn get_chunk(&mut self, chunk: &mut ItemChunk<I::Item>) -> Result<bool> {
            if let Some(item) = self.iter.next() {
                chunk.item = item;
                chunk.name = self.name.clone();
                Ok(false)
            } else {
                Ok(true)
            }
        }
    }

    // The offsets of each chunk, with its bytes.
    type ChunkDump = Vec<(Vec<u8>, Vec<u32>, Vec<u32>)>;

    fn dump_chunks<P: ChunkProducer<Chunk = OffsetChunk<Off>>, Off: ScanOffsets>(
        mut p: P,
        offsets: impl Fn(&Off) -> &Offsets,
    ) -> ChunkDump {
        let mut res = Vec::new();
        let mut chunk = OffsetChunk::<Off>::default();
        while !p.get_chunk(&mut chunk).expect("get_chunk should succeed") {
            chunk.finish(&mut OffsetChunk::default()).unwrap();
            let data = chunk.buf.as_ref().unwrap().as_bytes()[..chunk.len].to_vec();
            let off = offsets(&chunk.off);
            res.push((data, off.rel.fields.clone(), off.nl.fields.clone()));
        }
        res
    }

    fn deferred_inputs() -> Vec<Vec<u8>> {
        let mut long = Vec::new();
        for i in 0..200 {
            long.extend_from_slice(format!("w{i} ").as_bytes());
        }
        long.extend_from_slice(b"\nx y\n");
        vec![
            b"".to_vec(),
            b"a b\nc d e".to_vec(),
            b"a  b\t c\n\n  d\n".to_vec(),
            (0..500)
                .map(|i| format!("{i} f{} {}\n", i % 7, i % 3))
                .collect::<String>()
                .into_bytes(),
            long,
        ]
    }

    // A producer that leaves the scan to `Chunk::finish` yields the chunks and offsets of one
    // that scans itself.
    #[test]
    fn deferred_scan_matches_producer_scan() {
        use crate::runtime::splitter::batch::{
            get_find_indexes_ascii_whitespace, get_find_indexes_bytes,
        };
        for input in deferred_inputs() {
            for lines_only in [false, true] {
                for chunk_size in [16, 64, 4096] {
                    let bytes = |defer: bool, record_sep: u8| {
                        let lo = LinesOnly::new(AtomicBool::new(lines_only));
                        let mut p = new_offset_chunk_producer_bytes(
                            &input[..],
                            chunk_size,
                            "x",
                            b' ',
                            record_sep,
                            1,
                            false,
                            get_find_indexes_bytes(),
                            lo,
                        );
                        if defer {
                            p.defer_scan();
                        }
                        dump_chunks(p, |o: &Offsets| o)
                    };
                    for &record_sep in b"\n " {
                        assert_eq!(bytes(false, record_sep), bytes(true, record_sep));
                    }
                    let ws = |defer: bool| {
                        let lo = LinesOnly::new(AtomicBool::new(lines_only));
                        let mut p = new_offset_chunk_producer_ascii_whitespace(
                            &input[..],
                            chunk_size,
                            "x",
                            1,
                            false,
                            get_find_indexes_ascii_whitespace(),
                            lo,
                        );
                        if defer {
                            p.defer_scan();
                        }
                        dump_chunks(p, |o: &WhitespaceOffsets| &o.0)
                    };
                    let (a, b) = (ws(false), ws(true));
                    assert!(!input.is_empty() || a.is_empty());
                    for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
                        assert_eq!(
                            x,
                            y,
                            "chunk {i} of {}, lines_only={lines_only} chunk_size={chunk_size} data={:?}",
                            a.len(),
                            String::from_utf8_lossy(&x.0)
                        );
                    }
                    assert_eq!(a.len(), b.len());
                }
            }
        }
    }

    /// The input a CSV/TSV producer read, with the offsets of each chunk made absolute: the
    /// chunks themselves depend on where the producer cuts.
    fn csv_stream(
        input: &[u8],
        ifmt: InputFormat,
        chunk_size: usize,
        defer: bool,
    ) -> (Vec<u8>, Vec<u32>) {
        let mut p = new_offset_chunk_producer_csv(input, chunk_size, "x", ifmt, 1, false);
        if defer {
            p.defer_scan();
        }
        let (mut data, mut offs) = (Vec::new(), Vec::new());
        for (bs, rel, _) in dump_chunks(p, |o: &Offsets| o) {
            let base = data.len() as u32;
            // Every chunk but the last ends a record.
            offs.extend(rel.iter().map(|&o| base + o));
            data.extend_from_slice(&bs);
        }
        (data, offs)
    }

    // Leaving the CSV/TSV scan to the consumers gives the same offsets as the producer's scan.
    #[test]
    fn csv_deferred_scan_matches_producer_scan() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        const FIELDS: &[&str] = &[
            "abc",
            "",
            "\"q\"",
            "\"a,b\"",
            "\"multi\nline\nfield\"",
            "\"say \"\"hi\"\"\"",
            "\"crlf\r\nin\"",
            "tab\tand\\backslash",
            "0123456789abcdefghij0123456789abcdefghij",
        ];
        let mut inputs: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"a,b\n".to_vec(),
            b"a,\"b\nc\",d".to_vec(),
            // A quoted field longer than the chunks.
            format!("x,\"{}\"\ny\n", "long,\n".repeat(100)).into_bytes(),
        ];
        for _ in 0..60 {
            let mut buf = String::new();
            for _ in 0..next() % 300 {
                for j in 0..1 + next() % 6 {
                    if j > 0 {
                        buf.push(',');
                    }
                    buf.push_str(FIELDS[(next() % FIELDS.len() as u64) as usize]);
                }
                buf.push_str(if next() % 4 == 0 { "\r\n" } else { "\n" });
            }
            if next() % 2 == 0 {
                buf.push_str("last");
            }
            inputs.push(buf.into_bytes());
        }
        // The scan is left to the consumer, for quoted CSV too.
        let quoted = "a,\"b\nc\",\"\"\"d\"\n".repeat(100);
        for ifmt in [InputFormat::CSV, InputFormat::TSV] {
            let mut p = new_offset_chunk_producer_csv(quoted.as_bytes(), 64, "x", ifmt, 1, false);
            p.defer_scan();
            let mut chunk = OffsetChunk::default();
            assert!(!p.get_chunk(&mut chunk).unwrap());
            assert!(chunk.deferred.is_some() && chunk.off.rel.fields.is_empty());
            assert!(chunk.len < quoted.len());
        }
        for input in &inputs {
            for ifmt in [InputFormat::CSV, InputFormat::TSV] {
                for chunk_size in [16, 64, 4096] {
                    let want = csv_stream(input, ifmt, chunk_size, false);
                    assert_eq!(want.0, *input);
                    assert_eq!(
                        csv_stream(input, ifmt, chunk_size, true),
                        want,
                        "{ifmt:?} chunk_size={chunk_size} input={:?}",
                        String::from_utf8_lossy(input)
                    );
                }
            }
        }
        // Dirty data (quotes inside unquoted fields, text after closing quotes): the chunks are
        // cut where the quotes are known to be balanced, and the producer scans the others, so
        // all of the input is read.
        const ALPHABET: &[u8] = b"aaaa,,\"\"\n\n\r ";
        for _ in 0..300 {
            let input: Vec<u8> = (0..next() % 3000)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            for chunk_size in [16, 64, 4096] {
                let (data, _) = csv_stream(&input, InputFormat::CSV, chunk_size, true);
                assert_eq!(data, input);
            }
        }
    }

    // Reading into the buffers of consumed chunks (which hold the bytes of earlier chunks) yields
    // the same chunks and offsets as reading into new, zeroed, buffers.
    #[test]
    fn recycled_buffers_match_new_buffers() {
        use crate::runtime::splitter::batch::{
            get_find_indexes_ascii_whitespace, get_find_indexes_bytes,
        };
        // Drops the buffer of each chunk instead of handing it back to the producer.
        fn dump_fresh<P: ChunkProducer<Chunk = OffsetChunk<Off>>, Off: ScanOffsets>(
            mut p: P,
            offsets: impl Fn(&Off) -> &Offsets,
        ) -> ChunkDump {
            let mut res = Vec::new();
            loop {
                let mut chunk = OffsetChunk::<Off>::default();
                if p.get_chunk(&mut chunk).expect("get_chunk should succeed") {
                    return res;
                }
                chunk.finish(&mut OffsetChunk::default()).unwrap();
                let data = chunk.buf.as_ref().unwrap().as_bytes()[..chunk.len].to_vec();
                let off = offsets(&chunk.off);
                res.push((data, off.rel.fields.clone(), off.nl.fields.clone()));
            }
        }
        let mut inputs = deferred_inputs();
        // A short last chunk after full ones that leave non-whitespace bytes in the buffer.
        inputs.push(b"aaaaaaaaaaaaa bbbbbbbbbbbbbb\ncccccccccccccc dddddddddd\ne".to_vec());
        for input in inputs {
            for defer in [false, true] {
                for chunk_size in [16, 64, 4096] {
                    let bytes = || {
                        let mut p = new_offset_chunk_producer_bytes(
                            &input[..],
                            chunk_size,
                            "x",
                            b' ',
                            b'\n',
                            1,
                            false,
                            get_find_indexes_bytes(),
                            LinesOnly::new(AtomicBool::new(false)),
                        );
                        if defer {
                            p.defer_scan();
                        }
                        p
                    };
                    assert_eq!(
                        dump_chunks(bytes(), |o: &Offsets| o),
                        dump_fresh(bytes(), |o: &Offsets| o)
                    );
                    let ws = || {
                        let mut p = new_offset_chunk_producer_ascii_whitespace(
                            &input[..],
                            chunk_size,
                            "x",
                            1,
                            false,
                            get_find_indexes_ascii_whitespace(),
                            LinesOnly::new(AtomicBool::new(false)),
                        );
                        if defer {
                            p.defer_scan();
                        }
                        p
                    };
                    assert_eq!(
                        dump_chunks(ws(), |o: &WhitespaceOffsets| &o.0),
                        dump_fresh(ws(), |o: &WhitespaceOffsets| &o.0),
                        "defer={defer} chunk_size={chunk_size}"
                    );
                }
            }
        }
    }

    #[test]
    fn chained_all_elements() {
        let mut chained_producer = ChainedChunkProducer(vec![
            new_iter(20, 30, "file3")(),
            new_iter(10, 20, "file2")(),
            new_iter(0, 10, "file1")(),
        ]);
        let mut got = Vec::new();
        let mut names = Vec::new();
        let mut chunk = ItemChunk::default();
        let mut i = 0;
        while !chained_producer
            .get_chunk(&mut chunk)
            .expect("get_chunk should succeed")
        {
            if i % 10 == 0 {
                names.push(chunk.name.clone())
            }
            got.push(chunk.item);
            i += 1;
        }

        assert_eq!(got, (0..30).collect::<Vec<_>>());
        assert_eq!(
            names,
            vec![Arc::<str>::from("file1"), "file2".into(), "file3".into()]
        );
    }

    #[test]
    fn chained_next_file() {
        let mut chained_producer = ChainedChunkProducer(vec![
            new_iter(20, 30, "file3")(),
            new_iter(10, 20, "file2")(),
            new_iter(0, 10, "file1")(),
        ]);
        let mut got = Vec::new();
        let mut names = Vec::new();
        let mut chunk = ItemChunk::default();
        while !chained_producer
            .get_chunk(&mut chunk)
            .expect("get_chunk should succeed")
        {
            names.push(chunk.name.clone());
            got.push(chunk.item);
            if !chained_producer
                .next_file()
                .expect("next_file should succeed")
            {
                break;
            }
        }

        assert_eq!(got, vec![0, 10, 20]);
        assert_eq!(
            names,
            vec![Arc::<str>::from("file1"), "file2".into(), "file3".into()]
        );
    }

    #[test]
    fn sharded_next_file() {
        let mut sharded_producer = ShardedChunkProducer::new(
            vec![
                new_iter(0, 10, "file1"),
                new_iter(10, 20, "file2"),
                new_iter(20, 30, "file3"),
            ]
            .into_iter(),
        );
        let mut got = Vec::new();
        let mut names = Vec::new();
        let mut chunk = ItemChunk::default();
        while !sharded_producer
            .get_chunk(&mut chunk)
            .expect("get_chunk should succeed")
        {
            names.push(chunk.name.clone());
            got.push(chunk.item);
            if !sharded_producer
                .next_file()
                .expect("next_file should succeed")
            {
                break;
            }
        }

        assert_eq!(got, vec![0, 10, 20]);
        assert_eq!(
            names,
            vec![Arc::<str>::from("file1"), "file2".into(), "file3".into()]
        );
    }

    #[test]
    fn parallel_all_elements() {
        use std::{sync::Mutex, thread};
        let parallel_producer =
            ParallelChunkProducer::new(new_iter(0, 100, "file1"), /*chan_size=*/ 10);
        let got = Arc::new(Mutex::new(Vec::new()));
        let threads = {
            let _guard = got.lock().unwrap();
            let mut threads = Vec::with_capacity(5);
            for prod in parallel_producer.try_dyn_resize(5) {
                let got = got.clone();
                threads.push(thread::spawn(move || {
                    let mut prod = prod();
                    let mut chunk = ItemChunk::default();
                    while !prod
                        .get_chunk(&mut chunk)
                        .expect("get_chunk should succeed")
                    {
                        assert_eq!(chunk.name, Arc::<str>::from("file1"));
                        got.lock().unwrap().push(chunk.item);
                    }
                }));
            }
            threads
        };
        for t in threads.into_iter() {
            t.join().unwrap();
        }

        let mut g = got.lock().unwrap();
        g.sort();

        assert_eq!(*g, (0..100).collect::<Vec<_>>());
    }

    // Yields `items` chunks, then fails with `msg`.
    struct FailingChunkProducer {
        items: usize,
        msg: &'static str,
    }

    impl ChunkProducer for FailingChunkProducer {
        type Chunk = ItemChunk<usize>;
        fn next_file(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn get_chunk(&mut self, chunk: &mut ItemChunk<usize>) -> Result<bool> {
            if self.items == 0 {
                return err!("{}", self.msg);
            }
            self.items -= 1;
            chunk.item = self.items;
            Ok(false)
        }
    }

    #[test]
    fn parallel_propagates_errors() {
        use std::thread;
        let parallel_producer = ParallelChunkProducer::new(
            || FailingChunkProducer {
                items: 100,
                msg: "read failed",
            },
            /*chan_size=*/ 10,
        );
        let threads: Vec<_> = parallel_producer
            .try_dyn_resize(5)
            .into_iter()
            .map(|prod| {
                thread::spawn(move || {
                    let mut prod = prod();
                    let mut chunk = ItemChunk::default();
                    let mut got = 0;
                    loop {
                        match prod.get_chunk(&mut chunk) {
                            Ok(false) => got += 1,
                            Ok(true) => return (got, None),
                            Err(e) => return (got, Some(e.to_string())),
                        }
                    }
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        // Chunks read before the error are still delivered, and one consumer gets the error.
        assert_eq!(results.iter().map(|(got, _)| got).sum::<usize>(), 100);
        let errors: Vec<_> = results.iter().filter_map(|(_, e)| e.as_deref()).collect();
        assert_eq!(errors, vec!["read failed"]);
    }

    // An endless input that records when it is dropped (the input handle being closed).
    struct EndlessChunkProducer(Arc<AtomicBool>);

    impl Drop for EndlessChunkProducer {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl ChunkProducer for EndlessChunkProducer {
        type Chunk = ItemChunk<usize>;
        fn next_file(&mut self) -> Result<bool> {
            Ok(false)
        }
        fn get_chunk(&mut self, chunk: &mut ItemChunk<usize>) -> Result<bool> {
            chunk.item = 1;
            Ok(false)
        }
    }

    fn wait_for(flag: &AtomicBool) -> bool {
        for _ in 0..200 {
            if flag.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    // Once cancelled (e.g. by `exit`), the reading thread stops and releases its input, although
    // a consumer still holds the producer (the main thread keeps it until the process exits).
    #[test]
    fn parallel_cancel_stops_reader() {
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = CancelSignal::default();
        let d = dropped.clone();
        let mut prod = CancellableChunkProducer::new(
            signal.clone(),
            ParallelChunkProducer::new(move || EndlessChunkProducer(d), /*chan_size=*/ 2),
        );
        let mut chunk = ItemChunk::default();
        assert!(!prod.get_chunk(&mut chunk).unwrap());
        signal.cancel(0);
        assert!(prod.get_chunk(&mut chunk).unwrap());
        assert!(wait_for(&dropped), "the reading thread kept its input");
        drop(prod);
    }

    #[test]
    fn sharded_cancel_stops_feeder() {
        // The list of inputs (which may hold the inputs not read yet) records when it is dropped.
        struct Files(Arc<AtomicBool>);
        impl Drop for Files {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        impl Iterator for Files {
            type Item = Box<dyn FnOnce() -> IterChunkProducer<std::ops::Range<usize>> + Send>;
            fn next(&mut self) -> Option<Self::Item> {
                Some(Box::new(new_iter(0, 1, "file")))
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = CancelSignal::default();
        let mut prod = CancellableChunkProducer::new(
            signal.clone(),
            ShardedChunkProducer::new(Files(dropped.clone())),
        );
        let mut chunk = ItemChunk::default();
        assert!(!prod.get_chunk(&mut chunk).unwrap());
        signal.cancel(0);
        assert!(prod.get_chunk(&mut chunk).unwrap());
        assert!(wait_for(&dropped), "the feeding thread kept the inputs");
        drop(prod);
    }

    #[test]
    fn sharded_all_elements() {
        use std::{sync::Mutex, thread};
        let sharded_producer = ShardedChunkProducer::new(
            vec![
                new_iter(0, 10, "file1"),
                new_iter(10, 20, "file2"),
                new_iter(20, 30, "file3"),
                new_iter(30, 40, "file4"),
                new_iter(40, 50, "file5"),
                new_iter(50, 60, "file6"),
            ]
            .into_iter(),
        );
        let got = Arc::new(Mutex::new(Vec::new()));
        let threads = {
            let _guard = got.lock().unwrap();
            let mut threads = Vec::with_capacity(5);
            for prod in sharded_producer.try_dyn_resize(5) {
                let got = got.clone();
                threads.push(thread::spawn(move || {
                    let mut prod = prod();
                    let mut chunk = ItemChunk::default();
                    while !prod
                        .get_chunk(&mut chunk)
                        .expect("get_chunk should succeed")
                    {
                        let expected_name = match chunk.item {
                            0..=9 => "file1",
                            10..=19 => "file2",
                            20..=29 => "file3",
                            30..=39 => "file4",
                            40..=49 => "file5",
                            50..=59 => "file6",
                            x => panic!("unexpected item {} (should be in range [0,59])", x),
                        };
                        assert_eq!(&*chunk.name, expected_name);
                        got.lock().unwrap().push(chunk.item);
                    }
                }));
            }
            threads
        };
        for t in threads.into_iter() {
            t.join().unwrap();
        }

        let mut g = got.lock().unwrap();
        g.sort_unstable();

        assert_eq!(*g, (0..60).collect::<Vec<_>>());
    }

    // TODO: test that we get all elements in Chained, Sharded and Parallel chunkproducers.
    // TODO: test nextfile behavior for Chained and Sharded chunk producer.
}
