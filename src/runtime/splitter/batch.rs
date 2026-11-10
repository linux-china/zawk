//! Batched splitting of input.
//!
//! Under some circumstances, we can scan the entire input for field and record separators in
//! batches.
//!
//! CSV/TSV parsing is adapted from geofflangdale/simdcsv.
//!
//! As that repo name implies, it uses SIMD instructions to accelerate CSV and TSV parsing. It does
//! this by performing a fast pass over the input that produces a list of "relevant indices" into
//! the underlying input buffer, these play the same role as "control characters" in the SimdJSON
//! paper (by the same authors). The higher-level state machine parser then operates on these
//! control characters rather than on a byte-by-byte basis.
//!
//! Single-byte splitting is a simpler algorithm adapting the CSV/TSV parsing methods, but ignoring
//! escaping issues.

use std::io::Read;
use std::mem;
use std::sync::atomic::AtomicUsize;

use crate::common::{CancelSignal, ExecutionStrategy, Result};
use crate::pushdown::FieldSet;
use crate::runtime::{
    Int, RegexCache,
    str_impl::{Buf, DynamicBuf, Str, UniqueBuf},
};

use super::{
    DefaultLine, LineReader, ReaderState,
    chunk::{
        self, CancellableChunkProducer, Chunk, ChunkProducer, OffsetChunk, ParallelChunkProducer,
        ShardedChunkProducer,
    },
    display_name, field_from_end_offset, input_warning, normalize_join_indexes,
    resolve_negative_col,
};

pub struct CSVReader<P> {
    prod: P,
    cur_chunk: OffsetChunk,
    cur_buf: Buf,
    buf_len: usize,
    prev_ix: usize,
    last_len: usize,
    // Used to trigger updating FILENAME on the first read.
    ifmt: InputFormat,
    field_set: FieldSet,

    empty_buf: Buf,
    check_utf8: bool,
    // Number of records read from the current file, for warnings. None for the workers of a
    // parallel run, which do not see whole files.
    record_no: Option<usize>,
    // The format `getline < file` reads records in (see `LineReader::getline_format`).
    getline_format: Option<InputFormat>,
}

impl LineReader for CSVReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>> {
    type Line = Line;
    fn filename(&self) -> Str<'static> {
        Str::from(self.cur_chunk.get_name()).unmoor()
    }
    fn file_version(&self) -> u32 {
        self.cur_chunk.version
    }
    fn wait(&self) -> bool {
        self.prod.wait()
    }
    fn check_utf8(&self) -> bool {
        self.check_utf8
    }
    fn request_handles(&self, size: usize) -> Vec<Box<dyn FnOnce() -> Self + Send>> {
        let producers = self.prod.try_dyn_resize(size);
        let mut res = Vec::with_capacity(producers.len());
        let ifmt = self.ifmt;
        let getline_format = self.getline_format;
        for p_factory in producers.into_iter() {
            let field_set = self.field_set.clone();
            let check_utf8 = self.check_utf8;
            res.push(Box::new(move || {
                let empty_buf = UniqueBuf::new(0).into_buf();
                let cur_buf = empty_buf.clone();
                CSVReader {
                    prod: p_factory(),
                    cur_chunk: OffsetChunk::default(),
                    cur_buf,
                    empty_buf,
                    buf_len: 0,
                    prev_ix: 0,
                    last_len: 0,
                    ifmt,
                    field_set,
                    check_utf8,
                    record_no: None,
                    getline_format,
                }
            }) as _)
        }
        res
    }
    fn read_line(&mut self, _pat: &Str, _rc: &mut RegexCache) -> Result<(bool, Line)> {
        let mut line = Line::default();
        let changed = self.read_line_reuse(_pat, _rc, &mut line)?;
        Ok((changed, line))
    }
    fn read_line_reuse<'a, 'b: 'a>(
        &'b mut self,
        _pat: &Str,
        _rc: &mut RegexCache,
        old: &'a mut Line,
    ) -> Result<bool> {
        self.read_line_inner(old)
    }
    fn read_state(&self) -> i64 {
        if self.cur_chunk.version != 0 && self.last_len == 0 {
            ReaderState::Eof as i64
        } else {
            ReaderState::OK as i64
        }
    }
    fn next_file(&mut self) -> Result<bool> {
        self.cur_chunk.off.clear();
        self.cur_buf = UniqueBuf::new(0).into_buf();
        self.buf_len = 0;
        self.prev_ix = 0;
        self.prod.next_file()
    }
    fn set_used_fields(&mut self, field_set: &FieldSet) {
        self.field_set = field_set.clone();
    }
    fn getline_format(&self) -> Option<InputFormat> {
        self.getline_format
    }
}

impl CSVReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>> {
    pub fn new<I, S>(
        rs: I,
        ifmt: InputFormat,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + Send + 'static,
        S: Read + Send + 'static,
    {
        let prod: Box<dyn ChunkProducer<Chunk = OffsetChunk>> = match exec_strategy {
            ExecutionStrategy::Serial => Box::new(chunk::new_chained_offset_chunk_producer_csv(
                rs, chunk_size, ifmt, check_utf8,
            )),
            x @ ExecutionStrategy::ShardPerRecord(_) => {
                Box::new(CancellableChunkProducer::new(
                    cancel_signal,
                    ParallelChunkProducer::new(
                        move || {
                            chunk::new_chained_offset_chunk_producer_csv(
                                rs, chunk_size, ifmt, check_utf8,
                            )
                        },
                        /*channel_size*/ x.num_workers() * 2,
                    ),
                ))
            }
            ExecutionStrategy::ShardPerFile => {
                let iter = rs.enumerate().map(move |(i, (r, name))| {
                    move || {
                        chunk::new_offset_chunk_producer_csv(
                            r,
                            chunk_size,
                            name.as_str(),
                            ifmt,
                            i as u32 + 1,
                            check_utf8,
                        )
                    }
                });
                Box::new(CancellableChunkProducer::new(
                    cancel_signal,
                    ShardedChunkProducer::new(iter),
                ))
            }
        };
        let empty_buf = UniqueBuf::new(0).into_buf();
        let cur_buf = empty_buf.clone();
        CSVReader {
            prod,
            cur_buf,
            buf_len: 0,
            cur_chunk: OffsetChunk::default(),
            prev_ix: 0,
            last_len: 0,
            field_set: FieldSet::all(),
            ifmt,
            empty_buf,
            check_utf8,
            record_no: Some(0),
            getline_format: Some(ifmt),
        }
    }

    /// Reads `getline < file` as plain lines, for an input that is converted to CSV (e.g. a
    /// Markdown table) rather than given as CSV.
    pub fn plain_getline(mut self) -> Self {
        self.getline_format = None;
        self
    }
}

// TODO rename as it handles CSV and TSV
impl<P: ChunkProducer<Chunk = OffsetChunk>> CSVReader<P> {
    fn refresh_buf(&mut self) -> Result<(/*is eof*/ bool, /* file changed */ bool)> {
        let prev_version = self.cur_chunk.version;
        let placeholder = self.empty_buf.clone();
        let old_buf = mem::replace(&mut self.cur_buf, placeholder);
        self.buf_len = 0;

        // Send the chunks back, if possible, so they get freed in the same thread.
        self.cur_chunk.buf = old_buf.try_unique().ok();
        if self.prod.get_chunk(&mut self.cur_chunk)? {
            // We may have received an EOF without getting any data. Increment the version so this
            // registers as an EOF through the `read_state` interface.
            self.cur_chunk.version = std::cmp::max(prev_version, 1);
            return Ok((true, false));
        }
        self.cur_buf = self.cur_chunk.buf.take().unwrap().into_buf();
        self.buf_len = self.cur_chunk.len;
        self.prev_ix = 0;
        Ok((false, prev_version != self.cur_chunk.version))
    }

    fn stepper<'a, 'b: 'a>(&'b mut self, st: State, line: &'a mut Line) -> Stepper<'a> {
        line.ifmt = self.ifmt;
        Stepper {
            buf: &self.cur_buf,
            buf_len: self.buf_len,
            off: &mut self.cur_chunk.off,
            prev_ix: self.prev_ix,
            ifmt: self.ifmt,
            field_set: self.field_set.clone(),
            line,
            st,
        }
    }
    pub fn read_line_inner<'a, 'b: 'a>(
        &'b mut self,
        line: &'a mut Line,
    ) -> Result</*file changed*/ bool> {
        line.clear();
        let mut changed = false;
        if self.cur_chunk.off.rel.start == self.cur_chunk.off.rel.fields.len() {
            // NB: see comment on corresponding condition in ByteReader.
            let (is_eof, has_changed) = self.refresh_buf()?;
            changed = has_changed;
            // NB: >= because the `push_past` logic in stepper can result in prev_ix pointing two
            // past the end of the buffer.
            if is_eof && self.prev_ix >= self.buf_len {
                self.last_len = 0;
                debug_assert!(!changed);
                return Ok(false);
            }
        }

        let (prev_ix, st) = {
            let mut stepper = self.stepper(State::Init, line);
            (unsafe { stepper.step() }, stepper.st)
        };
        let consumed = prev_ix - self.prev_ix;
        self.prev_ix = prev_ix;
        self.last_len = consumed;
        self.record_no = self.record_no.map(|n| if changed { 1 } else { n + 1 });
        if st == State::Quote {
            // Chunks only end inside a quoted field at the end of the file: the quote is never
            // closed and the field has swallowed the rest of the input.
            static UNTERMINATED: AtomicUsize = AtomicUsize::new(0);
            let name = self.cur_chunk.get_name();
            let loc = || match self.record_no {
                Some(n) => format!("{}: record {}", display_name(name), n),
                None => display_name(name).to_string(),
            };
            input_warning(
                &UNTERMINATED,
                loc,
                "unterminated quoted field runs to the end of the file",
            );
        }
        if st != State::Done {
            line.promote();
        }
        Ok(changed)
    }
}

#[derive(Default, Debug)]
pub struct OffsetInner {
    pub start: usize,
    // Offsets are relative to the start of the chunk: the producers reject chunks of more than
    // `MAX_CHUNK_LEN` bytes, so `u32` is enough and halves the memory and write bandwidth of u64.
    // An alternative option would be to save lines and fields in separate vectors, but this is
    // more efficient for iteration
    pub fields: Vec<u32>,
}

/// The largest chunk whose offsets fit in [OffsetInner::fields].
pub const MAX_CHUNK_LEN: usize = u32::MAX as usize;

impl OffsetInner {
    pub(crate) fn clear(&mut self) {
        self.start = 0;
        self.fields.clear();
    }
}

// Structurally identical to WhitespaceOffsets, but newline metadata here is "optional"; it will
// contain a subset of the data contained in the "relevant" offsetse in "rel".
#[derive(Default, Debug)]
pub struct Offsets {
    pub rel: OffsetInner,
    pub nl: OffsetInner,
    /// Only the record separators were found (in `rel` for single-byte splitting, in `nl` for
    /// whitespace splitting): records are read without their fields, which `DefaultLine` splits
    /// from `$0` if they are used after all.
    pub lines_only: bool,
    /// Where the last SIMD scan can be continued (set by `generic::find_indexes`): when a chunk
    /// holds no complete record, the producer reads a larger buffer starting with the same bytes,
    /// and only the bytes from the last whole vector before the end of the old buffer on have to
    /// be scanned. Reset when the offsets are rewritten otherwise (e.g. by the lenient CSV scan).
    pub checkpoint: Resume,
    /// Armed by [Offsets::continue_scan] and consumed by the next scan.
    pub resume: Resume,
}

/// The state of a scan at a vector boundary (see [Offsets::resume]). `ix == 0` means there is
/// nothing to resume: the next scan starts over.
#[derive(Default, Debug, Clone, Copy)]
pub struct Resume {
    /// The byte offset at which to continue; a multiple of the kernel's vector size.
    pub ix: usize,
    /// The lengths of `rel.fields` and `nl.fields` for the bytes before `ix`.
    pub rel: usize,
    pub nl: usize,
    /// The kernel's carried state (quote parity, trailing `\r`, whitespace, ...) at `ix`.
    pub state: [u64; 6],
}

// Newtype wrapper, because chunks are processed slightly differently in the whitespace case.
#[derive(Default, Debug)]
pub struct WhitespaceOffsets(pub Offsets);

impl Offsets {
    pub(crate) fn clear(&mut self) {
        self.rel.clear();
        self.nl.clear();
        self.checkpoint = Resume::default();
        self.resume = Resume::default();
    }

    /// Make the next scan continue from [Offsets::checkpoint] instead of starting over. The
    /// buffer it scans must start with the bytes of the previous scan.
    pub(crate) fn continue_scan(&mut self) {
        self.resume = self.checkpoint;
    }
}

#[derive(Default, Clone, Debug)]
pub struct Line {
    raw: Str<'static>,
    // Why is len a separate value from raw? We don't always populate raw, but we need to report
    // line lengths to maintain some invariants from the Reader type in splitter.rs.
    len: usize,
    fields: Vec<Str<'static>>,
    partial: Str<'static>,
    ifmt: InputFormat,
    // A field or NF was assigned since `raw` was last set: `$0` must be rebuilt from the fields.
    diverged: bool,
    // A field or NF was assigned since the record was read or `$0` was assigned; unlike
    // `diverged`, this stays set after `$0` is rebuilt (see `Line::record`).
    assigned: bool,
}

impl Line {
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn len(&self) -> usize {
        self.len
    }
}

impl<'a> super::Line<'a> for Line {
    fn join_cols<F>(
        &mut self,
        start: Int,
        end: Int,
        sep: &Str<'a>,
        nf: usize,
        trans: F,
    ) -> Result<Str<'a>>
    where
        F: FnMut(Str<'static>) -> Str<'static>,
    {
        debug_assert_eq!(self.fields.len(), nf);
        let (start, end) = normalize_join_indexes(start, end, nf)?;
        let sep = sep.clone().unmoor();
        Ok(sep
            .join(self.fields[start..end].iter().cloned().map(trans))
            .upcast())
    }
    fn nf(&mut self, _pat: &Str, _rc: &mut super::RegexCache) -> Result<usize> {
        Ok(self.fields.len())
    }

    fn get_col(
        &mut self,
        col: super::Int,
        _pat: &Str,
        ofs: &Str,
        _rc: &mut super::RegexCache,
    ) -> Result<Str<'a>> {
        if col == 0 {
            if self.diverged {
                self.raw = ofs.join_slice(&self.fields[..]).unmoor();
                self.diverged = false;
            }
            return Ok(self.raw.clone().upcast());
        }
        let col = if col < 0 {
            match resolve_negative_col(col, self.fields.len())? {
                Some(col) => col,
                None => return Ok(Str::default().upcast()),
            }
        } else {
            col
        };
        Ok(self
            .fields
            .get(col as usize - 1)
            .cloned()
            .unwrap_or_default()
            .upcast())
    }

    fn set_col(
        &mut self,
        col: super::Int,
        s: &Str<'a>,
        _pat: &Str,
        _rc: &mut super::RegexCache,
    ) -> Result<()> {
        let col = if col < 0 {
            match resolve_negative_col(col, self.fields.len())? {
                Some(c) => c,
                None => {
                    return err!(
                        "attempt to assign field $(-{}); only {} fields",
                        field_from_end_offset(col),
                        self.fields.len()
                    );
                }
            }
        } else {
            col
        };
        if col == 0 {
            // Assigning `$0` splits it again with the input format.
            self.raw = s.clone().unmoor();
            self.fields = self.raw.with_bytes(|bs| split_record(bs, self.ifmt));
            self.diverged = false;
            self.assigned = false;
            return Ok(());
        }
        let col = col as usize - 1;
        if col >= self.fields.len() {
            self.fields.resize_with(col + 1, Str::default);
        }
        self.fields[col] = s.clone().unmoor();
        self.diverged = true;
        self.assigned = true;
        Ok(())
    }
    fn set_nf(&mut self, nf: super::Int, _pat: &Str, _rc: &mut super::RegexCache) -> Result<()> {
        if nf < 0 {
            return err!("NF set to negative value {}", nf);
        }
        self.fields.resize_with(nf as usize, Str::default);
        self.diverged = true;
        self.assigned = true;
        Ok(())
    }
    fn fields_assigned(&self) -> bool {
        self.assigned
    }
    fn record_is_row(&self, sep: &Str<'a>) -> bool {
        sep.with_bytes(|bs| bs == [self.ifmt.sep()])
    }
}

/// Split a single CSV or TSV record, with the same rules as [Stepper::step].
fn split_record(bs: &[u8], ifmt: InputFormat) -> Vec<Str<'static>> {
    let bs = match bs {
        [rest @ .., b'\r', b'\n'] | [rest @ .., b'\n'] => rest,
        _ => bs,
    };
    let mut fields = Vec::new();
    if bs.is_empty() {
        return fields;
    }
    let sep = ifmt.sep();
    let mut cur = Vec::new();
    let mut i = 0;
    while i < bs.len() {
        let b = bs[i];
        i += 1;
        match (ifmt, b) {
            _ if b == sep => fields.push(Buf::read_from_bytes(&mem::take(&mut cur)).into_str()),
            (InputFormat::CSV, b'"') => {
                // A quoted portion runs to the next lone quote; `""` is an escaped quote.
                while i < bs.len() {
                    if bs[i] == b'"' {
                        if bs.get(i + 1) == Some(&b'"') {
                            cur.push(b'"');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    cur.push(bs[i]);
                    i += 1;
                }
            }
            (InputFormat::TSV, b'\\') => {
                match bs.get(i) {
                    Some(b'n') => cur.push(b'\n'),
                    Some(b't') => cur.push(b'\t'),
                    Some(b'r') => cur.push(b'\r'),
                    Some(b'\\') => cur.push(b'\\'),
                    // A backslash before a separator or at the end is kept as is.
                    Some(b'\t') | None => {
                        cur.push(b'\\');
                        continue;
                    }
                    Some(&x) => cur.extend_from_slice(&[b'\\', x]),
                }
                i += 1;
            }
            _ => cur.push(b),
        }
    }
    fields.push(Buf::read_from_bytes(&cur).into_str());
    fields
}

impl Line {
    pub fn promote(&mut self) {
        let partial = mem::take(&mut self.partial);
        self.fields.push(partial);
    }
    pub fn promote_null(&mut self) {
        debug_assert_eq!(self.partial, Str::default());
        self.fields.push(Str::default());
    }
    pub fn clear(&mut self) {
        self.fields.clear();
        self.partial = Str::default();
        self.raw = Str::default();
        self.len = 0;
        self.diverged = false;
        self.assigned = false;
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum State {
    Init,
    BS,
    Quote,
    QuoteInQuote,
    Done,
}

// Stepper implements the core splitting algorithm given a "chunk" of offsets computed by a
// CSVReader. The [step] method implements a basic state machine going through the control
// characters extracted by initial pass.
pub struct Stepper<'a> {
    pub ifmt: InputFormat,
    pub buf: &'a Buf,
    pub buf_len: usize,
    pub off: &'a mut Offsets,
    pub prev_ix: usize,
    pub st: State,
    pub line: &'a mut Line,
    pub field_set: FieldSet,
}

impl<'a> Stepper<'a> {
    fn append(&mut self, s: Str<'static>) {
        let partial = mem::take(&mut self.line.partial);
        self.line.partial = Str::concat(partial, s);
    }

    unsafe fn append_slice(&mut self, i: usize, j: usize) {
        self.append(self.buf.slice_to_str(i, j));
    }

    unsafe fn push_past(&mut self, i: usize) {
        self.append_slice(self.prev_ix, i);
        self.prev_ix = i + 1;
    }

    pub fn promote_null(&mut self) {
        self.line.promote_null();
    }
    pub fn promote(&mut self) {
        self.line.promote();
    }

    // Whether the record ending at the line end `ix` is empty (or a lone `\r` of a `\r\n`).
    unsafe fn is_blank_line(&self, bs: &[u8], line_start: usize, ix: usize) -> bool {
        self.line.len == 0
            && self.line.fields.is_empty()
            && (ix == line_start
                || (ix == line_start + 1 && *bs.get_unchecked(line_start) == b'\r'))
    }

    fn get(&mut self, line_start: usize, j: usize, cur: usize) -> usize {
        self.off.rel.start = cur;
        if self.field_set.get(0) {
            // Drop the `\r` of a `\r\n` line ending from `$0`, as the fields already do (and as
            // gawk `--csv` does). `len` still counts it: it tracks the bytes consumed.
            let end = if j > line_start && self.buf.as_bytes()[j - 1] == b'\r' {
                j - 1
            } else {
                j
            };
            self.line.raw = self.buf.slice_to_str(line_start, end);
        }
        self.line.len += j - line_start;
        self.prev_ix
    }

    pub(crate) unsafe fn step(&mut self) -> usize {
        let sep = self.ifmt.sep();
        let line_start = self.prev_ix;
        let bs = &self.buf.as_bytes()[0..self.buf_len];
        let mut cur = self.off.rel.start;
        macro_rules! get_next {
            () => {
                if cur == self.off.rel.fields.len() {
                    self.push_past(bs.len());
                    return self.get(line_start, bs.len(), cur);
                } else {
                    let res = *self.off.rel.fields.get_unchecked(cur) as usize;
                    cur += 1;
                    res
                }
            };
        }
        'outer: loop {
            match self.st {
                State::Init => 'init: loop {
                    // First, skip over any unused fields.
                    let cur_field = self.line.fields.len() + 1;
                    if !self.field_set.get(cur_field) {
                        // Parity of the quotes seen in this field: odd at the end of the input
                        // means a quoted field that is never closed.
                        let mut open_quote = false;
                        loop {
                            if cur == self.off.rel.fields.len() {
                                self.prev_ix = bs.len() + 1;
                                if open_quote {
                                    self.st = State::Quote;
                                }
                                return self.get(line_start, bs.len(), cur);
                            }
                            let ix = *self.off.rel.fields.get_unchecked(cur) as usize;
                            cur += 1;
                            match *bs.get_unchecked(ix) {
                                b'"' => open_quote = !open_quote,
                                b'\r' | b'\\' => {}
                                b'\n' => {
                                    self.prev_ix = ix + 1;
                                    if !self.is_blank_line(bs, line_start, ix) {
                                        self.promote_null();
                                    }
                                    self.st = State::Done;
                                    return self.get(line_start, ix, cur);
                                }
                                _x => {
                                    debug_assert_eq!(_x, sep);
                                    self.prev_ix = ix + 1;
                                    self.promote_null();
                                    continue 'init;
                                }
                            }
                        }
                    }
                    // Common case: Loop through records until the end of the line.
                    let ix = get_next!();
                    match *bs.get_unchecked(ix) {
                        b'\r' => {
                            // Drop the \r of a \r\n line ending; a lone \r is field data.
                            if ix + 1 >= bs.len() || *bs.get_unchecked(ix + 1) == b'\n' {
                                self.push_past(ix);
                            }
                            continue;
                        }
                        b'\n' => {
                            // TSV does not report `\r`: drop the `\r` of a `\r\n` line ending
                            // here (CSV already did in the `\r` arm above).
                            if ix > self.prev_ix && *bs.get_unchecked(ix - 1) == b'\r' {
                                self.append_slice(self.prev_ix, ix - 1);
                                self.prev_ix = ix + 1;
                            } else {
                                self.push_past(ix);
                            }
                            // A blank line has no fields (NF == 0), like with the default FS.
                            if !self.is_blank_line(bs, line_start, ix) {
                                self.promote();
                            }
                            self.st = State::Done;
                            return self.get(line_start, ix, cur);
                        }
                        b'"' => {
                            self.push_past(ix);
                            self.st = State::Quote;
                            continue 'outer;
                        }
                        // Only happens in TSV mode
                        b'\\' => {
                            self.push_past(ix);
                            self.st = State::BS;
                            continue 'outer;
                        }
                        _x => {
                            debug_assert_eq!(_x, sep);
                            self.push_past(ix);
                            self.promote();
                            continue;
                        }
                    }
                },
                State::Quote => {
                    // Parse a quoted field; this will only happen in CSV mode.
                    let ix = get_next!();
                    match *bs.get_unchecked(ix) {
                        b'"' => {
                            // We have found a quote, time to figure out if the next character is a
                            // quote, or if it is the end of the quoted portion of the field.
                            //
                            // One interesting thing to note here is that this allows for a mixture
                            // of quoted and unquoted portions of a single CSV field, which is
                            // technically more than is supported by the standard IIUC.
                            self.push_past(ix);
                            self.st = State::QuoteInQuote;
                            continue;
                        }
                        // Line ends inside a quoted field are only reported by
                        // `find_indexes_csv_lenient`, after a literal quote: the record ends here.
                        b'\r' => {
                            self.push_past(ix);
                            continue;
                        }
                        b'\n' => {
                            self.push_past(ix);
                            self.promote();
                            self.st = State::Done;
                            return self.get(line_start, ix, cur);
                        }
                        _ => unreachable!(),
                    }
                }
                State::QuoteInQuote => {
                    // We've just seen a " inside a ", it could be the end of the quote, or it
                    // could be an escaped quote. We peek ahead one character and check.
                    if bs.len() == self.prev_ix {
                        // We are past the end! Let's pick this up later.
                        // We had better not have any more offsets in the stream!
                        debug_assert_eq!(self.off.rel.fields.len(), cur);
                        return self.get(line_start, bs.len(), cur);
                    }
                    if *bs.get_unchecked(self.prev_ix) == b'"' {
                        self.append("\"".into());
                        self.st = State::Quote;
                        // burn the next entry. It should be a quote. Using get_next here is a
                        // convenience: if we hit the branch that returns early within the macro,
                        // there's a bug somewhere! But there shouldn't be one, because all quotes
                        // should appear in the offsets vector, and we know that there is more
                        // space in `bs`.
                        let _q = get_next!();
                        debug_assert_eq!(bs[_q], b'"');
                        self.prev_ix += 1;
                    } else {
                        self.st = State::Init;
                    }
                }
                State::BS => {
                    if bs.len() == self.prev_ix {
                        debug_assert_eq!(self.off.rel.fields.len(), cur);
                        return self.get(line_start, bs.len(), cur);
                    }
                    match *bs.get_unchecked(self.prev_ix) {
                        b'n' => self.append("\n".into()),
                        b't' => self.append("\t".into()),
                        b'r' => self.append("\r".into()),
                        b'\\' => {
                            self.append("\\".into());
                            // The escaped backslash is in the offsets too: burn it, or it would
                            // start another escape.
                            let _b = get_next!();
                            debug_assert_eq!(_b, self.prev_ix);
                        }
                        b'\t' | b'\n' => {
                            // A backslash before a separator or line end is kept as is, and the
                            // separator is handled as usual.
                            self.append("\\".into());
                            self.st = State::Init;
                            continue;
                        }
                        x => {
                            // Unknown escape: keep the backslash and the raw byte. Build from
                            // bytes, since `x` may not be valid UTF-8 on its own.
                            self.append(Buf::read_from_bytes(&[b'\\', x]).into_str());
                        }
                    }
                    self.prev_ix += 1;
                    // Backslash escapes only exist in TSV, which has no quoted fields.
                    self.st = State::Init;
                }
                State::Done => panic!("cannot start in Done state"),
            }
        }
    }
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub enum InputFormat {
    #[default]
    CSV,
    TSV,
}

impl InputFormat {
    fn sep(self) -> u8 {
        match self {
            InputFormat::CSV => b',',
            InputFormat::TSV => b'\t',
        }
    }
}

// get_find_indexes{_bytes,_ascii_whitespace}, what's that all about?
//
// These functions use vector instructions that, while commonly supported on x86, are occasionally
// missing. The safest way to handle this fact is to query _at runtime_ whether or not a given
// feature-set is supported. To avoid querying this on every function call, the calling library
// will instead store a function pointer that is computed at startup based on the dynamically
// available CPU features.

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,pclmulqdq")]
unsafe fn find_indexes_csv_avx2(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    generic::find_indexes_csv::<avx2::Impl>(buf, offsets, prev_iter_inside_quote, prev_iter_cr_end)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,pclmulqdq")]
unsafe fn find_indexes_tsv_avx2(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    generic::find_indexes_tsv::<avx2::Impl>(buf, offsets, prev_iter_inside_quote, prev_iter_cr_end)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2,pclmulqdq")]
unsafe fn find_indexes_csv_sse2(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    generic::find_indexes_csv::<sse2::Impl>(buf, offsets, prev_iter_inside_quote, prev_iter_cr_end)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2,pclmulqdq")]
unsafe fn find_indexes_tsv_sse2(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    generic::find_indexes_tsv::<sse2::Impl>(buf, offsets, prev_iter_inside_quote, prev_iter_cr_end)
}

#[cfg(target_arch = "aarch64")]
unsafe fn find_indexes_csv_neon(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    unsafe {
        generic::find_indexes_csv::<neon::Impl>(
            buf,
            offsets,
            prev_iter_inside_quote,
            prev_iter_cr_end,
        )
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn find_indexes_tsv_neon(
    buf: &[u8],
    offsets: &mut Offsets,
    prev_iter_inside_quote: u64,
    prev_iter_cr_end: u64,
) -> (u64, u64) {
    unsafe {
        generic::find_indexes_tsv::<neon::Impl>(
            buf,
            offsets,
            prev_iter_inside_quote,
            prev_iter_cr_end,
        )
    }
}

pub fn get_find_indexes(
    ifmt: InputFormat,
) -> unsafe fn(&[u8], &mut Offsets, u64, u64) -> (u64, u64) {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "x86_64")] {
            #[cfg(feature = "allow_avx2")]
            const ALLOW_AVX2: bool = true;
            #[cfg(not(feature = "allow_avx2"))]
            const ALLOW_AVX2: bool = false;
            if ALLOW_AVX2 && is_x86_feature_detected!("avx2") && is_x86_feature_detected!("pclmulqdq") {
                 match ifmt {
                     InputFormat::CSV => find_indexes_csv_avx2,
                     InputFormat::TSV => find_indexes_tsv_avx2,
                 }
             } else if is_x86_feature_detected!("sse2") && is_x86_feature_detected!("pclmulqdq") {
                 match ifmt {
                     InputFormat::CSV => find_indexes_csv_sse2,
                     InputFormat::TSV => find_indexes_tsv_sse2,
                 }
             } else {
                 match ifmt {
                     InputFormat::CSV => generic::find_indexes_csv::<generic::Impl>,
                     InputFormat::TSV => generic::find_indexes_tsv::<generic::Impl>,
                 }
             }
        } else if #[cfg(target_arch = "aarch64")] {
            match ifmt {
                InputFormat::CSV => find_indexes_csv_neon,
                InputFormat::TSV => find_indexes_tsv_neon,
            }
        } else {
            match ifmt {
                InputFormat::CSV => generic::find_indexes_csv::<generic::Impl>,
                InputFormat::TSV => generic::find_indexes_tsv::<generic::Impl>,
            }
        }
    }
}

/// Byte-by-byte CSV scan for buffers with quotes that the SIMD pass cannot handle by parity.
///
/// Follows gawk: only a `"` at the start of a field opens a quoted field, and inside one a `"`
/// followed by anything other than `"`, `,`, `\n` or `\r\n` is a literal quote. Literal quotes
/// are not reported, so the [Stepper] copies them as field data; the offsets are otherwise the
/// same as the SIMD pass would produce. After a literal quote, a newline outside quotes (by
/// parity) ends the field and the record, as gawk does.
#[cfg(test)]
fn find_indexes_csv_lenient(buf: &[u8], offsets: &mut Offsets) {
    offsets.clear();
    csv_lenient_scan(buf, &mut offsets.rel.fields, 0, usize::MAX);
}

/// The scan of [find_indexes_csv_lenient] from `from`, which must be the start of a record,
/// appending the offsets to `fields`. It stops at the first record start at or after `stop_at`
/// and returns it (`buf.len()` if it reaches the end): the state there (outside quotes, at a
/// field start, no `\r` pending) is the initial state of a fresh SIMD scan.
fn csv_lenient_scan(buf: &[u8], fields: &mut Vec<u32>, from: usize, stop_at: usize) -> usize {
    let len = buf.len();
    let mut field_start = true;
    let mut i = from;
    while i < len {
        match buf[i] {
            b',' => {
                fields.push(i as u32);
                field_start = true;
            }
            b'\n' => {
                fields.push(i as u32);
                field_start = true;
                if i + 1 >= stop_at {
                    return i + 1;
                }
            }
            b'\r' => {
                fields.push(i as u32);
                field_start = false;
            }
            b'"' if field_start => {
                fields.push(i as u32);
                i += 1;
                // Once a literal quote shows up, records end as in gawk: at a newline outside
                // quotes by parity, counting every quote. Without this, a single malformed field
                // would swallow the rest of the input.
                let mut outside = false;
                loop {
                    let found = if outside {
                        memchr::memchr2(b'"', b'\n', &buf[i..])
                    } else {
                        memchr::memchr(b'"', &buf[i..])
                    };
                    let Some(j) = found else {
                        // Unterminated: the rest of the buffer is inside the quotes.
                        return len;
                    };
                    let q = i + j;
                    if buf[q] == b'\n' {
                        // End the field and the record here; the [Stepper] sees the line end
                        // while still inside the quoted field.
                        if q > 0 && buf[q - 1] == b'\r' {
                            fields.push(q as u32 - 1);
                        }
                        fields.push(q as u32);
                        field_start = true;
                        i = q + 1;
                        if i >= stop_at {
                            return i;
                        }
                        break;
                    }
                    i = q + 1;
                    match buf.get(q + 1) {
                        Some(b'"') => {
                            fields.extend_from_slice(&[q as u32, q as u32 + 1]);
                            i = q + 2;
                        }
                        None | Some(b',' | b'\n') => {
                            // The closing quote; the separator or line end after it is handled
                            // next.
                            fields.push(q as u32);
                            field_start = false;
                            break;
                        }
                        Some(b'\r') if matches!(buf.get(q + 2), None | Some(b'\n')) => {
                            fields.push(q as u32);
                            field_start = false;
                            break;
                        }
                        // A literal quote inside the quoted field.
                        Some(_) => outside = !outside,
                    }
                }
                continue;
            }
            _ => field_start = false,
        }
        i += 1;
    }
    len
}

pub type BytesIndexKernel = unsafe fn(&[u8], &mut Offsets, u8, u8);

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_indexes_byte_avx2(buf: &[u8], offsets: &mut Offsets, field_sep: u8, record_sep: u8) {
    generic::find_indexes_byte::<avx2::Impl>(buf, offsets, field_sep, record_sep)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn find_indexes_byte_sse2(buf: &[u8], offsets: &mut Offsets, field_sep: u8, record_sep: u8) {
    generic::find_indexes_byte::<sse2::Impl>(buf, offsets, field_sep, record_sep)
}

pub fn get_find_indexes_bytes() -> BytesIndexKernel {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "x86_64")] {
            #[cfg(feature = "allow_avx2")]
            const ALLOW_AVX2: bool = true;
            #[cfg(not(feature = "allow_avx2"))]
            const ALLOW_AVX2: bool = false;
            if ALLOW_AVX2 && is_x86_feature_detected!("avx2") {
                find_indexes_byte_avx2
            } else if is_x86_feature_detected!("sse2") {
                find_indexes_byte_sse2
            } else {
                generic::find_indexes_byte::<generic::Impl>
            }
        } else if #[cfg(target_arch = "aarch64")] {
            generic::find_indexes_byte::<neon::Impl>
        } else {
            generic::find_indexes_byte::<generic::Impl>
        }
    }
}

pub type WhitespaceIndexKernel = unsafe fn(&[u8], &mut WhitespaceOffsets, u64) -> u64;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn find_indexes_ascii_whitespace_avx2(
    buf: &[u8],
    offsets: &mut WhitespaceOffsets,
    start_ws: u64,
) -> u64 {
    generic::find_indexes_ascii_whitespace::<avx2::Impl>(buf, offsets, start_ws)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn find_indexes_ascii_whitespace_sse2(
    buf: &[u8],
    offsets: &mut WhitespaceOffsets,
    start_ws: u64,
) -> u64 {
    generic::find_indexes_ascii_whitespace::<sse2::Impl>(buf, offsets, start_ws)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn find_indexes_ascii_whitespace_ssse3(
    buf: &[u8],
    offsets: &mut WhitespaceOffsets,
    start_ws: u64,
) -> u64 {
    generic::find_indexes_ascii_whitespace::<sse2::Impl<true>>(buf, offsets, start_ws)
}

pub fn get_find_indexes_ascii_whitespace() -> WhitespaceIndexKernel {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "x86_64")] {
            #[cfg(feature = "allow_avx2")]
            const ALLOW_AVX2: bool = true;
            #[cfg(not(feature = "allow_avx2"))]
            const ALLOW_AVX2: bool = false;
            if ALLOW_AVX2 && is_x86_feature_detected!("avx2") {
                find_indexes_ascii_whitespace_avx2
            } else if is_x86_feature_detected!("ssse3") {
                find_indexes_ascii_whitespace_ssse3
            } else if is_x86_feature_detected!("sse2") {
                find_indexes_ascii_whitespace_sse2
            } else {
                generic::find_indexes_ascii_whitespace::<generic::Impl>
            }
        } else if #[cfg(target_arch = "aarch64")] {
            generic::find_indexes_ascii_whitespace::<neon::Impl>
        } else {
            generic::find_indexes_ascii_whitespace::<generic::Impl>
        }
    }
}

/// Whether an RFC 4180 CSV field must be quoted: it contains a quote, comma or line break, or has
/// leading or trailing spaces that other tools could trim.
pub fn csv_needs_quotes(bs: &[u8]) -> bool {
    matches!(bs.first(), Some(b' ' | b'\t'))
        || matches!(bs.last(), Some(b' ' | b'\t'))
        || if bs.len() < 32 {
            // A `memchr` call costs more than a short byte loop.
            bs.iter().any(|b| matches!(b, b'"' | b',' | b'\n' | b'\r'))
        } else {
            memchr::memchr3(b'"', b',', b'\n', bs).is_some() || memchr::memchr(b'\r', bs).is_some()
        }
}

/// Escape a field per RFC 4180: quote it if needed and double any quotes. Line breaks and tabs
/// are kept as is inside the quotes.
pub fn escape_csv<'a>(s: &Str<'a>) -> Str<'a> {
    use std::io::Write;
    s.with_bytes(|bs| {
        if !csv_needs_quotes(bs) {
            return s.clone();
        }
        // One pass: copy the runs between quotes, writing each quote twice (the buffer grows
        // on the rare field with many quotes).
        let mut buf = DynamicBuf::new(bs.len() + 4);
        buf.write_all(b"\"").unwrap();
        let mut start = 0;
        for q in memchr::memchr_iter(b'"', bs) {
            buf.write_all(&bs[start..=q]).unwrap();
            buf.write_all(b"\"").unwrap();
            start = q + 1;
        }
        buf.write_all(&bs[start..]).unwrap();
        buf.write_all(b"\"").unwrap();
        buf.into_str()
    })
}

/// Escape a field for `-o tsv`: a backslash, tab, newline or carriage return becomes `\\`, `\t`,
/// `\n` or `\r`, the escapes that `-i tsv` reverses. The `--null` text is written as `\N`.
pub fn escape_tsv<'a>(s: &Str<'a>) -> Str<'a> {
    use crate::runtime::str_escape::{TSV_NULL, is_null};
    use std::io::Write;
    s.with_bytes(|bs| {
        if is_null(bs) {
            return Str::from(TSV_NULL);
        }
        let escapes = bs
            .iter()
            .filter(|b| matches!(b, b'\\' | b'\t' | b'\n' | b'\r'))
            .count();
        if escapes == 0 {
            return s.clone();
        }
        let mut buf = DynamicBuf::new(bs.len() + escapes);
        for b in bs {
            let esc: &[u8] = match b {
                b'\\' => br"\\",
                b'\t' => br"\t",
                b'\n' => br"\n",
                b'\r' => br"\r",
                _ => std::slice::from_ref(b),
            };
            buf.write_all(esc).unwrap();
        }
        buf.into_str()
    })
}

#[cfg(test)]
mod escape_tests {
    use super::*;

    #[test]
    fn csv_escaping() {
        let cases = [
            ("no escaping", "no escaping"),
            ("tab\tonly", "tab\tonly"),
            ("a, b", r#""a, b""#),
            (r#"say "hi""#, r#""say ""hi""""#),
            ("multi\nline", "\"multi\nline\""),
            ("cr\rhere", "\"cr\rhere\""),
            (" padded", "\" padded\""),
            (r"C:\new\table", r"C:\new\table"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(
                escape_csv(&Str::from(input)),
                Str::from(expected),
                "input: {:?}",
                input
            );
        }
    }

    #[test]
    fn tsv_escaping() {
        let s1 = Str::from("no, escaping");
        let s2 = Str::from("This ought to be escaped, for one\treason");
        assert_eq!(escape_tsv(&s1), s1);
        assert_eq!(
            escape_tsv(&s2),
            Str::from(r#"This ought to be escaped, for one\treason"#)
        );
        assert_eq!(escape_tsv(&Str::from("a\\nb\r\n")), Str::from(r"a\\nb\r\n"));
    }
}

mod generic {
    use super::{MAX_CHUNK_LEN, Offsets, Resume, WhitespaceOffsets};
    use std::mem;
    const MAX_INPUT_SIZE: usize = 64;

    pub trait Vector: Copy {
        const VEC_BYTES: usize;
        const INPUT_SIZE: usize = Self::VEC_BYTES * 2;
        const _ASSERT_LTE_MAX: usize = MAX_INPUT_SIZE - Self::INPUT_SIZE;

        // Precondition: bptr points to at least INPUT_SIZE bytes.
        unsafe fn fill_input(btr: *const u8) -> Self;
        unsafe fn or(self, rhs: Self) -> Self;
        #[allow(dead_code)]
        unsafe fn and(self, rhs: Self) -> Self;
        unsafe fn mask(self) -> u64;
        // Compute a mask of which bits in input match (bytewise) `m`.
        unsafe fn cmp_against_input(self, m: u8) -> Self;

        #[inline(always)]
        unsafe fn cmp_mask_against_input(self, m: u8) -> u64 {
            self.cmp_against_input(m).mask()
        }

        unsafe fn find_quote_mask(
            self,
            prev_iter_inside_quote: &mut u64,
        ) -> (/*inside quotes*/ u64, /*quote locations*/ u64);

        // SIMD splitting by whitespace.
        //
        // whitespace_masks outputs (a) the position of newlines in the input and (b) the location
        // of field delimiters. Unlike, say, byte-based splitting, the end of one field is not
        // necessarily the start of the next field. To allow for this, the returned whitespace mask
        // contains both the start and end offsets for individual fields. For example,
        //
        // [the raven  caws]
        //  ^  ^^    ^ ^
        // should have the offsets [0, 3, 4, 9, 11]. These are read off the offsets stream in
        // pairs, with special handling for the end of the stream, and for newlines that land in
        // the middle of a run of whitespace.
        //
        // We find these whitespace runs by computing masks for the presence of whitespace
        // characters, then shifting by 1 and computing an XOR. In our above example
        //
        // Input:            [the raven  caws]
        // Whitespace Mask:  [000100000110000]
        // Shifted by 1:     [100010000011000]
        //
        // Where the extra 1 bit is governed by the start_ws variable. This is initialized to 1
        // and otherwise takes on the value that is "shifted off" of the previous input (0, in this
        // case).
        //
        // Xor of the two:   [100110000101000]
        //
        // Which provides us with the offsets that we need. This sequence is straightforward
        // to implement with no branches, and using SIMD instructions that are fairly cheap on
        // recent x86 hardware.
        #[inline(always)]
        unsafe fn whitespace_masks(
            self,
            start_ws: u64,
        ) -> (
            /* whitespace runs */ u64,
            /* newlines */ u64,
            /* next start_ws */ u64,
        ) {
            let (ws1, nl) = self.whitespace_and_newline_masks();
            let mut ws2 = ws1.wrapping_shl(1) | start_ws;
            if Self::INPUT_SIZE != 64 {
                ws2 &= !(1 << Self::INPUT_SIZE as u32)
            }
            let ws_res = ws1 ^ ws2;
            let next_start_ws = ws1.wrapping_shr(Self::INPUT_SIZE as u32 - 1);
            (ws_res, nl, next_start_ws)
        }

        /// Masks of the ASCII whitespace bytes (` `, `\t`, `\n`, `\r`) and of `\n` alone. SIMD
        /// implementations with a byte shuffle override this with a table lookup (see
        /// [WS_BY_LOW_NIBBLE]).
        #[inline(always)]
        unsafe fn whitespace_and_newline_masks(self) -> (u64, u64) {
            cmp_whitespace_and_newline_masks(self)
        }
    }

    /// Four compares and three ORs: the fallback for vectors without a byte shuffle.
    #[inline(always)]
    pub unsafe fn cmp_whitespace_and_newline_masks<V: Vector>(v: V) -> (u64, u64) {
        let nl = v.cmp_against_input(b'\n');
        let ws = v
            .cmp_against_input(b' ')
            .or(v.cmp_against_input(b'\t'))
            .or(nl)
            .or(v.cmp_against_input(b'\r'));
        (ws.mask(), nl.mask())
    }

    /// Whitespace classification by one table lookup (the simdjson trick). The four whitespace
    /// bytes have distinct low nibbles (`' '` = 0x20, `\t` = 0x09, `\n` = 0x0A, `\r` = 0x0D), so
    /// entry `i` holds the whitespace byte whose low nibble is `i`, and a byte `x` is whitespace
    /// iff `table[x & 0xF] == x`. The other entries are 0, which never matches: a byte with low
    /// nibble `i != 0` is not 0. Bytes >= 0x80 look up 0 as well (`pshufb` zeroes lanes whose index
    /// has the top bit set; NEON masks the index to the low nibble first).
    pub const WS_BY_LOW_NIBBLE: [u8; 16] =
        [b' ', 0, 0, 0, 0, 0, 0, 0, 0, b'\t', b'\n', 0, 0, b'\r', 0, 0];

    #[derive(Copy, Clone)]
    pub struct Impl([u8; 32]);

    macro_rules! foreach_impl_inner {
        ($ix: ident, $body:expr, [$($ixv:expr),*] ) => {{
            Impl([ $( foreach_impl_inner!($ix, $body, $ixv) ),* ])
        }};
        ($ix: ident, $body:expr, $ixv:expr) => {{
            let $ix = $ixv;
            $body
        }};
    }

    macro_rules! foreach_impl {
        ($ix:ident, $body:expr) => {
            foreach_impl_inner!(
                $ix,
                $body,
                [
                    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21,
                    22, 23, 24, 25, 26, 27, 28, 29, 30, 31
                ]
            )
        };
    }

    // A generic implementation of the `Vector` trait. No explicit simd, and relatively few unsafe
    // constructs aside from the explicitly unsafe `fill_input`.
    impl Vector for Impl {
        const VEC_BYTES: usize = 32;
        const INPUT_SIZE: usize = Self::VEC_BYTES;

        unsafe fn fill_input(btr: *const u8) -> Self {
            use std::ptr::copy;
            let mut i = Impl([0; 32]);
            copy(btr, i.0.as_mut_ptr(), Self::VEC_BYTES);
            i
        }

        unsafe fn or(self, rhs: Self) -> Self {
            foreach_impl!(ix, self.0[ix] | rhs.0[ix])
        }

        unsafe fn and(self, rhs: Self) -> Self {
            foreach_impl!(ix, self.0[ix] & rhs.0[ix])
        }

        unsafe fn mask(self) -> u64 {
            let mut res = 0u64;
            for i in 0..Self::VEC_BYTES {
                res |= (self.0[i] as u64 & 1) << i
            }
            res
        }

        unsafe fn cmp_against_input(self, m: u8) -> Self {
            foreach_impl!(ix, if self.0[ix] == m { 1u8 } else { 0u8 })
        }

        unsafe fn find_quote_mask(self, prev_iter_inside_quote: &mut u64) -> (u64, u64) {
            let quote_mask = self.cmp_against_input(b'"').mask();
            // Prefix XOR: bit i of `res` is the parity of the quotes at positions 0..=i.
            let mut res = quote_mask;
            res ^= res << 1;
            res ^= res << 2;
            res ^= res << 4;
            res ^= res << 8;
            res ^= res << 16;
            res ^= res << 32;
            let in_quotes_mask = res ^ *prev_iter_inside_quote;
            *prev_iter_inside_quote = (in_quotes_mask as i64).wrapping_shr(63) as u64;
            (in_quotes_mask, quote_mask)
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub unsafe fn default_x86_find_quote_mask<V: Vector>(
        inp: V,
        prev_iter_inside_quote: &mut u64,
    ) -> (/*inside quotes*/ u64, /*quote locations*/ u64) {
        use std::arch::x86_64::*;
        // This is about finding a mask that has 1s for all characters inside a quoted pair, plus
        // the starting quote, but not the ending one. For example:
        // [unquoted text "quoted text"]
        // has the mask
        // [000000000000001111111111110]
        // We will use this mask to avoid splitting on commas that are inside a quoted field. We
        // start by generating a mask for all the quote characters appearing in the string.
        let quote_bits = inp.cmp_mask_against_input(b'"');
        // Then we pull this trick from the simdjson paper. Lets use the example from the comments
        // above:
        // [unquoted text "quoted text"]
        // quoted_bits is going to be
        // [000000000000001000000000001]
        // We want to fill anything between the ones with ones. One way to do this is to replace
        // bit i with the prefix sum mod 2 (ie xor) of all the bits up to that point. We could do
        // this by repeated squaring, but intel actually just gives us this instruction for free:
        // it's "carryless multiplication" by all 1s. Caryless multiplication takes the prefix xor
        // of a each bit up to bit i, and-ed by corresponding bits in the other operand; anding
        // with all 1s gives us the sum we want.
        let mut quote_mask =
            _mm_cvtsi128_si64(/* grab the lower 64 bits */ _mm_clmulepi64_si128(
                /*pad with zeros*/ _mm_set_epi64x(0i64, quote_bits as i64),
                /*all 1s*/ _mm_set1_epi8(!0),
                0,
            )) as u64;
        let prev = *prev_iter_inside_quote;
        // We need to invert the mask if we started off inside a quote.
        quote_mask ^= prev;
        // We want all 1s if we ended in a quote, all zeros if not
        *prev_iter_inside_quote = (quote_mask as i64).wrapping_shr(63) as u64;
        (quote_mask, quote_bits)
    }

    #[inline(always)]
    unsafe fn flatten_bits(
        base_p: *mut u32,
        start_offset: &mut usize,
        start_ix: u32,
        mut bits: u64,
    ) {
        // We want to write indexes of set bits into the array base_p.
        // start_offset is the initial offset into base_p where we start writing.
        // start_ix is the index into the corpus pointed to by the initial bits of `bits`.
        if bits == 0 {
            return;
        }
        let count = bits.count_ones();
        let next_start = (*start_offset) + count as usize;
        // Unroll the loop for the first 8 bits. As in simdjson, we are potentially "overwriting"
        // here. We do not know if we have 8 bits to write, but we are writing them anyway! This is
        // "safe" so long as we have enough space in the array pointed to by base_p because:
        // * Excessive writes will only write a zero
        // * we will set next start to point to the right location.
        // Why do the extra work? We want to avoid extra branches.
        macro_rules! write_offset_inner {
            ($ix:expr) => {
                *base_p.add(*start_offset + $ix) = start_ix + bits.trailing_zeros();
                bits = bits & bits.wrapping_sub(1);
            };
        }
        macro_rules! write_offsets {
             ($($ix:expr),*) => { $(write_offset_inner!($ix);)* };
        }
        write_offsets!(0, 1, 2, 3, 4, 5, 6, 7);
        // Similarly for bits 8->16
        if count > 8 {
            write_offsets!(8, 9, 10, 11, 12, 13, 14, 15);
        }
        // Just do a loop for the last 48 bits.
        if count > 16 {
            *start_offset += 16;
            loop {
                write_offsets!(0);
                if bits == 0 {
                    break;
                }
                *start_offset += 1;
            }
        }
        *start_offset = next_start;
    }

    /// Make room for `room` more entries after the first `len` (initialized) entries of `v`, and
    /// return its (possibly moved) buffer. The capacity grows with the number of offsets actually
    /// found, rather than being reserved up front for one offset per input byte.
    #[inline(always)]
    unsafe fn ensure_room(v: &mut Vec<u32>, len: usize, room: usize) -> *mut u32 {
        if v.capacity() - len < room {
            // `reserve` only keeps the first `v.len()` entries.
            unsafe { v.set_len(len) };
            v.reserve(room);
        }
        v.as_mut_ptr()
    }

    /// The kernel state carried from one vector to the next, saved in [Resume] to continue a scan.
    pub trait ScanState: Copy {
        fn save(self) -> [u64; 6];
        fn load(saved: [u64; 6]) -> Self;
    }

    impl ScanState for () {
        fn save(self) -> [u64; 6] {
            [0; 6]
        }
        fn load(_: [u64; 6]) -> Self {}
    }

    impl ScanState for u64 {
        fn save(self) -> [u64; 6] {
            [self, 0, 0, 0, 0, 0]
        }
        fn load(saved: [u64; 6]) -> Self {
            saved[0]
        }
    }

    impl ScanState for (u64, u64, u64, u64, u64, u64) {
        fn save(self) -> [u64; 6] {
            [self.0, self.1, self.2, self.3, self.4, self.5]
        }
        fn load(s: [u64; 6]) -> Self {
            (s[0], s[1], s[2], s[3], s[4], s[5])
        }
    }

    // The find_indexes functions are variations on "do a vectorized comparison against a byte
    // sequence, write the indexes of matching indexes into Offsets." The first is very close to
    // the simd-csv variant; simpler formats do a bit less.

    // TODO: CSV/TSV currently do not make use of the whitespace offsets, but collecting them
    // doesn't appear to add much overhead, and they are used to get substantial speedups in the
    // ByteSplitter case.
    //
    // We should see about rearchitecting the TSV algorithm to make better use of sparse rows,
    // where lots of columns are present in the inputs, but only a few are used.

    /// Scan `buf`, starting over unless `offsets.resume` was armed (see
    /// [Offsets::continue_scan]), in which case `buf` must start with the bytes of the previous
    /// scan and only the bytes from `offsets.resume.ix` on are scanned, from the saved state.
    /// Either way, `offsets.checkpoint` is set to the state at the last whole vector, from which
    /// a scan of a longer buffer can continue.
    pub unsafe fn find_indexes<V, F, S>(buf: &[u8], offsets: &mut Offsets, state: S, f: F) -> S
    where
        V: Vector,
        S: ScanState,
        F: FnMut(S, *const u8) -> (S, /* rel */ u64, /* newlines */ u64),
    {
        find_indexes_from::<V, F, S, _>(buf, offsets, 0, state, f, |_| false).0
    }

    /// The vectors [find_indexes_from] scans at a time.
    pub const BUFFER_SIZE: usize = 4;

    /// [find_indexes], generalized in two ways:
    ///
    /// * With `from > 0`, the scan starts at `buf[from]` from `state` and appends to the offsets
    ///   already there (`offsets.resume` is ignored and `offsets.checkpoint` left alone).
    /// * After each group of vectors (up to [BUFFER_SIZE] of them) the scan stops as soon as
    ///   `stop(&state)` holds, and returns the index of the first byte of that group. The offsets
    ///   of the group are dropped, so the ones kept are exactly those before that index, and
    ///   `offsets.checkpoint` is reset.
    pub unsafe fn find_indexes_from<V, F, S, G>(
        buf: &[u8],
        offsets: &mut Offsets,
        from: usize,
        mut state: S,
        mut f: F,
        stop: G,
    ) -> (S, Option<usize>)
    where
        V: Vector,
        S: ScanState,
        F: FnMut(S, *const u8) -> (S, /* rel */ u64, /* newlines */ u64),
        G: Fn(&S) -> bool,
    {
        // flatten_bits writes up to max(popcount, 16) entries from its start offset, so a batch
        // of BUFFER_SIZE vectors writes at most this many entries.
        const ROOM: usize = MAX_INPUT_SIZE * BUFFER_SIZE + 16;
        assert!(
            buf.len() <= MAX_CHUNK_LEN,
            "chunk of {} bytes is too large",
            buf.len()
        );
        let resume = if from == 0 {
            mem::take(&mut offsets.resume)
        } else {
            Resume::default()
        };
        let mut ix = if from != 0 {
            debug_assert!(from <= buf.len());
            from
        } else if resume.ix != 0 && resume.ix <= buf.len() {
            debug_assert_eq!(resume.ix % V::INPUT_SIZE, 0);
            offsets.rel.start = 0;
            offsets.nl.start = 0;
            offsets.rel.fields.truncate(resume.rel);
            offsets.nl.fields.truncate(resume.nl);
            state = S::load(resume.state);
            resume.ix
        } else {
            offsets.clear();
            0
        };
        let buf_ptr = buf.as_ptr();
        let len = buf.len();
        let len_minus_64 = len.saturating_sub(V::INPUT_SIZE);
        let mut field_base = offsets.rel.fields.len();
        let mut newline_base = offsets.nl.fields.len();
        let mut field_base_ptr: *mut u32;
        let mut newline_base_ptr: *mut u32;
        macro_rules! ensure_room {
            () => {{
                field_base_ptr = ensure_room(&mut offsets.rel.fields, field_base, ROOM);
                newline_base_ptr = ensure_room(&mut offsets.nl.fields, newline_base, ROOM);
            }};
        }
        macro_rules! iterate {
            ($buf:expr) => {{
                #[cfg(feature = "unstable")]
                std::intrinsics::prefetch_read_data($buf.offset(128), 3);
                let (s, mask, nl) = f(state, $buf);
                state = s;
                (mask, nl)
            }};
        }
        macro_rules! check_stop {
            () => {
                if stop(&state) {
                    offsets.rel.fields.set_len(field_base);
                    offsets.nl.fields.set_len(newline_base);
                    if from == 0 {
                        offsets.checkpoint = Resume::default();
                    }
                    return (state, Some(ix));
                }
            };
        }
        if len_minus_64 > V::INPUT_SIZE * BUFFER_SIZE {
            let mut fields = [0u64; BUFFER_SIZE];
            let mut nls = [0u64; BUFFER_SIZE];
            while ix < len_minus_64 - V::INPUT_SIZE * BUFFER_SIZE + 1 {
                for b in 0..BUFFER_SIZE {
                    let (rel, nl) = iterate!(buf_ptr.add(V::INPUT_SIZE * b + ix));
                    fields[b] = rel;
                    nls[b] = nl;
                }
                check_stop!();
                ensure_room!();
                for b in 0..BUFFER_SIZE {
                    let internal_ix = (V::INPUT_SIZE * b + ix) as u32;
                    flatten_bits(field_base_ptr, &mut field_base, internal_ix, fields[b]);
                    flatten_bits(newline_base_ptr, &mut newline_base, internal_ix, nls[b]);
                }
                ix += V::INPUT_SIZE * BUFFER_SIZE;
            }
        }
        // Do an unbuffered version for the remaining data
        while ix < len_minus_64 {
            let (fields, nl) = iterate!(buf_ptr.add(ix));
            check_stop!();
            ensure_room!();
            flatten_bits(field_base_ptr, &mut field_base, ix as u32, fields);
            flatten_bits(newline_base_ptr, &mut newline_base, ix as u32, nl);
            ix += V::INPUT_SIZE;
        }
        // The rest is scanned with zero padding, so the state after it does not carry over to a
        // longer buffer: a continued scan starts from here.
        let checkpoint = Resume {
            ix,
            rel: field_base,
            nl: newline_base,
            state: state.save(),
        };
        // For any text that remains, just copy the results to the stack with some padding and do
        // one more iteration.
        let remaining = len - ix;
        if remaining > 0 {
            let mut rest = [0u8; MAX_INPUT_SIZE];
            std::ptr::copy_nonoverlapping(buf_ptr.add(ix), rest.as_mut_ptr(), remaining);
            let (fields, nl) = iterate!(rest.as_mut_ptr());
            check_stop!();
            ensure_room!();
            flatten_bits(field_base_ptr, &mut field_base, ix as u32, fields);
            flatten_bits(newline_base_ptr, &mut newline_base, ix as u32, nl);
        }
        debug_assert!(field_base <= offsets.rel.fields.capacity());
        debug_assert!(newline_base <= offsets.nl.fields.capacity());
        offsets.rel.fields.set_len(field_base);
        offsets.nl.fields.set_len(newline_base);
        if from == 0 {
            offsets.checkpoint = checkpoint;
        }
        (state, None)
    }

    pub unsafe fn find_indexes_csv<V: Vector>(
        buf: &[u8],
        offsets: &mut Offsets,
        prev_iter_inside_quote: u64, /*start at 0*/
        prev_iter_cr_end: u64,       /*start at 0*/
    ) -> (u64, u64) {
        // Quote state is tracked by parity, which is only right when every opening quote starts a
        // field and every closing quote ends one. Alongside the offsets we check exactly that:
        // an opening quote must follow the start of the buffer, a separator, a newline or a
        // closing quote (the `""` escape); a closing quote must be followed by a quote, a
        // separator, `\n` or `\r\n`. Any other quote ("irregular") stops the SIMD scan: the
        // record holding it is redone with the byte-by-byte `csv_lenient_scan`, which hands back
        // to the SIMD scan at a record start some way after it (see `LENIENT_SPAN`).
        let valid = if V::INPUT_SIZE == 64 {
            !0u64
        } else {
            (1u64 << V::INPUT_SIZE) - 1
        };
        let last = V::INPUT_SIZE as u32 - 1;
        let f = |(
            mut prev_iter_inside_quote,
            mut prev_iter_cr_end,
            mut prev_field_end,
            mut prev_close,
            mut prev_close_cr,
            mut irregular,
        ): (u64, u64, u64, u64, u64, u64),
                 buf| {
            let inp = V::fill_input(buf);
            let (quote_mask, quote_locs) = inp.find_quote_mask(&mut prev_iter_inside_quote);
            let sep = inp.cmp_mask_against_input(b',');

            let cr = inp.cmp_mask_against_input(0x0d);
            let cr_adjusted = cr.wrapping_shl(1) | prev_iter_cr_end;
            let lf = inp.cmp_mask_against_input(0x0a);
            // Allow for either \r\n or \n.
            let end = (lf & cr_adjusted) | lf;
            prev_iter_cr_end = cr.wrapping_shr(last);
            // NB: for now, NL is going to be unused for csv
            // Don't use NL here for now
            // let nl = end & !quote_mask;
            // RFC 4180: the only escape inside a quoted field is "", so backslashes are plain
            // data in CSV and are not reported (unlike TSV).
            let mask = ((sep | cr | end) & !quote_mask) | quote_locs;

            let opening = quote_locs & quote_mask;
            let closing = quote_locs & !quote_mask;
            let field_end = sep | lf | quote_locs;
            let field_start = field_end.wrapping_shl(1) | prev_field_end;
            let after_close = (closing.wrapping_shl(1) | prev_close) & valid;
            let close_cr = after_close & cr;
            let after_close_cr = (close_cr.wrapping_shl(1) | prev_close_cr) & valid;
            irregular |= (opening & !field_start)
                | (after_close & !(field_end | cr))
                | (after_close_cr & !lf);
            prev_field_end = field_end.wrapping_shr(last) & 1;
            prev_close = closing.wrapping_shr(last) & 1;
            prev_close_cr = close_cr.wrapping_shr(last) & 1;
            (
                (
                    prev_iter_inside_quote,
                    prev_iter_cr_end,
                    prev_field_end,
                    prev_close,
                    prev_close_cr,
                    irregular,
                ),
                mask,
                0,
            )
        };
        let irregular = |s: &(u64, u64, u64, u64, u64, u64)| s.5 != 0;
        let (state, mut stopped) = find_indexes_from::<V, _, _, _>(
            buf,
            offsets,
            0,
            (prev_iter_inside_quote, prev_iter_cr_end, 1, 0, 0, 0),
            f,
            irregular,
        );
        if stopped.is_none() {
            return (state.0, state.1);
        }
        // The SIMD scans that stopped all started at a record start, outside quotes.
        let len = buf.len();
        let mut seg_start = 0;
        let mut span = LENIENT_SPAN;
        while let Some(group) = stopped {
            // The first irregular quote is in the group starting at `group` (an irregular byte
            // after a closing quote can be up to 2 bytes after the quote), and every offset
            // before it is right. Back up to the start of its record: past the last newline
            // before it, which is outside quotes like every newline among the offsets.
            let lo = group.saturating_sub(2).max(seg_start);
            let fields = &mut offsets.rel.fields;
            let before = fields.partition_point(|&o| (o as usize) < lo);
            let rec_start = fields[..before]
                .iter()
                .rposition(|&o| buf[o as usize] == b'\n')
                .map_or(seg_start, |k| fields[k] as usize + 1)
                .max(seg_start);
            let keep = fields.partition_point(|&o| (o as usize) < rec_start);
            fields.truncate(keep);
            let stop_at = group + V::INPUT_SIZE * BUFFER_SIZE + span;
            let resume_at = super::csv_lenient_scan(buf, fields, rec_start, stop_at);
            if resume_at >= len {
                break;
            }
            let (_, next) = find_indexes_from::<V, _, _, _>(
                buf,
                offsets,
                resume_at,
                (0, 0, 1, 0, 0, 0),
                f,
                irregular,
            );
            // Dirty data with irregular quotes close together: scan more of it byte by byte
            // before trying the SIMD scan again.
            span = match next {
                Some(g) if g - resume_at < span => (span * 2).min(LENIENT_SPAN_MAX),
                _ => LENIENT_SPAN,
            };
            seg_start = resume_at;
            stopped = next;
        }
        // Only a scan of the whole buffer in one go can be continued.
        offsets.checkpoint = Resume::default();
        // The quote state is no longer tracked by parity.
        (0, 0)
    }

    /// How far (in bytes) past the group of vectors with an irregular quote the lenient CSV scan
    /// goes at least before handing back to the SIMD scan; it doubles, up to
    /// `LENIENT_SPAN_MAX`, while irregular quotes keep showing up within that distance.
    const LENIENT_SPAN: usize = 256;
    const LENIENT_SPAN_MAX: usize = 1 << 16;

    pub unsafe fn find_indexes_tsv<V: Vector>(
        buf: &[u8],
        offsets: &mut Offsets,
        // These two are ignored for TSV
        _prev_iter_inside_quote: u64,
        _prev_iter_cr_end: u64,
    ) -> (u64, u64) {
        find_indexes_unquoted::<V, _>(buf, offsets, |ptr| {
            let inp = V::fill_input(ptr);
            let sep = inp.cmp_against_input(b'\t');
            let esc = inp.cmp_against_input(b'\\');
            let lf = inp.cmp_against_input(b'\n');
            (sep.or(esc).or(lf).mask(), 0)
        });
        (0, 0)
    }

    pub unsafe fn find_indexes_unquoted<V: Vector, F: Fn(*const u8) -> (u64, u64)>(
        buf: &[u8],
        offsets: &mut Offsets,
        f: F,
    ) -> (u64, u64) {
        find_indexes::<V, _, _>(buf, offsets, (), |(), buf| {
            let (rel, nl) = f(buf);
            ((), rel, nl)
        });
        (0, 0)
    }

    // Unlike the other find_indexes methods, splitting by ASCII whitespace involves emitting two
    // streams of offsets: once for newlines and one for field boundaries (encapsulated in
    // WhitespaceOffsets). Other than that, the basic structure is the same.
    pub unsafe fn find_indexes_ascii_whitespace<V: Vector>(
        buf: &[u8],
        offsets: &mut WhitespaceOffsets,
        start_ws: u64, /*start at 1*/
    ) -> u64 /*next start ws*/ {
        find_indexes::<V, _, _>(buf, &mut offsets.0, start_ws, |start_ws, buf| {
            let inp = V::fill_input(buf);
            let (ws, nl, next_start) = inp.whitespace_masks(start_ws);
            (next_start, ws, nl)
        })
    }

    pub unsafe fn find_indexes_byte<V: Vector>(
        buf: &[u8],
        offsets: &mut Offsets,
        field_sep: u8,
        record_sep: u8,
    ) {
        find_indexes_unquoted::<V, _>(buf, offsets, |ptr| {
            let inp = V::fill_input(ptr);
            let fs = inp.cmp_against_input(field_sep);
            let rs = inp.cmp_against_input(record_sep);
            (fs.or(rs).mask(), rs.mask())
        });
    }
}

// #[target_feature(enable = "sse2")]

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use super::generic::{
        Vector, WS_BY_LOW_NIBBLE, cmp_whitespace_and_newline_masks, default_x86_find_quote_mask,
    };
    // This is in a large part based on geofflangdale/simdcsv, which is an adaptation of some of
    // the techniques from the simdjson paper (by that paper's authors, it appears) to CSV parsing.
    use std::arch::x86_64::*;
    /// `SSSE3` enables the `pshufb` whitespace lookup; callers must then run inside a function
    /// with `#[target_feature(enable = "ssse3")]`.
    #[derive(Copy, Clone)]
    pub struct Impl<const SSSE3: bool = false> {
        lo: __m128i,
        hi: __m128i,
    }

    impl<const SSSE3: bool> Vector for Impl<SSSE3> {
        const VEC_BYTES: usize = 16;
        #[inline(always)]
        unsafe fn fill_input(bptr: *const u8) -> Self {
            Self {
                lo: _mm_loadu_si128(bptr as *const _),
                hi: _mm_loadu_si128(bptr.add(Self::VEC_BYTES) as *const _),
            }
        }

        #[inline(always)]
        unsafe fn mask(self) -> u64 {
            let lo = _mm_movemask_epi8(self.lo) as u32 as u64;
            let hi = _mm_movemask_epi8(self.hi) as u32 as u64;
            lo | hi << Self::VEC_BYTES
        }

        #[inline(always)]
        unsafe fn or(self, rhs: Self) -> Self {
            let lo = _mm_or_si128(self.lo, rhs.lo);
            let hi = _mm_or_si128(self.hi, rhs.hi);
            Self { lo, hi }
        }

        #[inline(always)]
        unsafe fn and(self, rhs: Self) -> Self {
            let lo = _mm_and_si128(self.lo, rhs.lo);
            let hi = _mm_and_si128(self.hi, rhs.hi);
            Self { lo, hi }
        }

        #[inline(always)]
        unsafe fn cmp_against_input(self, m: u8) -> Self {
            // Load the mask into all lanes.
            let mask = _mm_set1_epi8(m as i8);
            let lo = _mm_cmpeq_epi8(self.lo, mask);
            let hi = _mm_cmpeq_epi8(self.hi, mask);
            Self { lo, hi }
        }

        #[inline(always)]
        unsafe fn find_quote_mask(
            self,
            prev_iter_inside_quote: &mut u64,
        ) -> (/*inside quotes*/ u64, /*quote locations*/ u64) {
            default_x86_find_quote_mask::<Self>(self, prev_iter_inside_quote)
        }

        #[inline(always)]
        unsafe fn whitespace_and_newline_masks(self) -> (u64, u64) {
            if !SSSE3 {
                return cmp_whitespace_and_newline_masks(self);
            }
            let table = _mm_loadu_si128(WS_BY_LOW_NIBBLE.as_ptr() as *const _);
            let ws = Self {
                lo: _mm_cmpeq_epi8(_mm_shuffle_epi8(table, self.lo), self.lo),
                hi: _mm_cmpeq_epi8(_mm_shuffle_epi8(table, self.hi), self.hi),
            };
            (ws.mask(), self.cmp_mask_against_input(b'\n'))
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use super::generic::{Vector, WS_BY_LOW_NIBBLE, default_x86_find_quote_mask};
    // This is in a large part based on geofflangdale/simdcsv, which is an adaptation of some of
    // the techniques from the simdjson paper (by that paper's authors, it appears) to CSV parsing.
    use std::arch::x86_64::*;
    #[derive(Copy, Clone)]
    pub struct Impl {
        lo: __m256i,
        hi: __m256i,
    }

    impl Vector for Impl {
        const VEC_BYTES: usize = 32;
        #[inline(always)]
        unsafe fn fill_input(bptr: *const u8) -> Self {
            Impl {
                lo: _mm256_loadu_si256(bptr as *const _),
                hi: _mm256_loadu_si256(bptr.add(Self::VEC_BYTES) as *const _),
            }
        }

        #[inline(always)]
        unsafe fn mask(self) -> u64 {
            let lo = _mm256_movemask_epi8(self.lo) as u32 as u64;
            let hi = _mm256_movemask_epi8(self.hi) as u32 as u64;
            lo | hi << Self::VEC_BYTES
        }

        #[inline(always)]
        unsafe fn or(self, rhs: Self) -> Self {
            let lo = _mm256_or_si256(self.lo, rhs.lo);
            let hi = _mm256_or_si256(self.hi, rhs.hi);
            Impl { lo, hi }
        }

        #[inline(always)]
        unsafe fn and(self, rhs: Self) -> Self {
            let lo = _mm256_and_si256(self.lo, rhs.lo);
            let hi = _mm256_and_si256(self.hi, rhs.hi);
            Impl { lo, hi }
        }

        #[inline(always)]
        unsafe fn cmp_against_input(self, m: u8) -> Self {
            // Load the mask into all lanes.
            let mask = _mm256_set1_epi8(m as i8);
            let lo = _mm256_cmpeq_epi8(self.lo, mask);
            let hi = _mm256_cmpeq_epi8(self.hi, mask);
            Impl { lo, hi }
        }

        #[inline(always)]
        unsafe fn find_quote_mask(
            self,
            prev_iter_inside_quote: &mut u64,
        ) -> (/*inside quotes*/ u64, /*quote locations*/ u64) {
            default_x86_find_quote_mask::<Self>(self, prev_iter_inside_quote)
        }

        #[inline(always)]
        unsafe fn whitespace_and_newline_masks(self) -> (u64, u64) {
            // vpshufb looks up within each 128-bit lane, so the table is in both lanes.
            let table =
                _mm256_broadcastsi128_si256(_mm_loadu_si128(WS_BY_LOW_NIBBLE.as_ptr() as *const _));
            let ws = Impl {
                lo: _mm256_cmpeq_epi8(_mm256_shuffle_epi8(table, self.lo), self.lo),
                hi: _mm256_cmpeq_epi8(_mm256_shuffle_epi8(table, self.hi), self.hi),
            };
            (ws.mask(), self.cmp_mask_against_input(b'\n'))
        }
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::generic::{Vector, WS_BY_LOW_NIBBLE};
    // NEON is part of the aarch64 baseline, so no runtime detection is needed.
    use std::arch::aarch64::*;

    /// 64 bytes at a time, as four 128-bit registers.
    #[derive(Copy, Clone)]
    pub struct Impl([uint8x16_t; 4]);

    impl Vector for Impl {
        const VEC_BYTES: usize = 32;

        #[inline(always)]
        unsafe fn fill_input(bptr: *const u8) -> Self {
            unsafe {
                Impl([
                    vld1q_u8(bptr),
                    vld1q_u8(bptr.add(16)),
                    vld1q_u8(bptr.add(32)),
                    vld1q_u8(bptr.add(48)),
                ])
            }
        }

        #[inline(always)]
        unsafe fn or(self, rhs: Self) -> Self {
            let [a, b, c, d] = self.0;
            let [e, f, g, h] = rhs.0;
            unsafe {
                Impl([
                    vorrq_u8(a, e),
                    vorrq_u8(b, f),
                    vorrq_u8(c, g),
                    vorrq_u8(d, h),
                ])
            }
        }

        #[inline(always)]
        unsafe fn and(self, rhs: Self) -> Self {
            let [a, b, c, d] = self.0;
            let [e, f, g, h] = rhs.0;
            unsafe {
                Impl([
                    vandq_u8(a, e),
                    vandq_u8(b, f),
                    vandq_u8(c, g),
                    vandq_u8(d, h),
                ])
            }
        }

        // The lanes are all ones or all zeros (comparison results). As in simdjson: keep one bit
        // weight per lane, then add neighboring lanes three times, which packs the 64 lanes into
        // the 64 bits of the low half.
        #[inline(always)]
        unsafe fn mask(self) -> u64 {
            const WEIGHTS: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
            unsafe {
                let w = vld1q_u8(WEIGHTS.as_ptr());
                let [a, b, c, d] = self.0;
                let ab = vpaddq_u8(vandq_u8(a, w), vandq_u8(b, w));
                let cd = vpaddq_u8(vandq_u8(c, w), vandq_u8(d, w));
                let s = vpaddq_u8(ab, cd);
                let s = vpaddq_u8(s, s);
                vgetq_lane_u64::<0>(vreinterpretq_u64_u8(s))
            }
        }

        #[inline(always)]
        unsafe fn cmp_against_input(self, m: u8) -> Self {
            let [a, b, c, d] = self.0;
            unsafe {
                let m = vdupq_n_u8(m);
                Impl([
                    vceqq_u8(a, m),
                    vceqq_u8(b, m),
                    vceqq_u8(c, m),
                    vceqq_u8(d, m),
                ])
            }
        }

        #[inline(always)]
        unsafe fn find_quote_mask(
            self,
            prev_iter_inside_quote: &mut u64,
        ) -> (/*inside quotes*/ u64, /*quote locations*/ u64) {
            let quote_bits = unsafe { self.cmp_mask_against_input(b'"') };
            // Prefix XOR: bit i is the parity of the quotes at positions 0..=i. With the `aes`
            // feature (Apple Silicon, Graviton, ...) this is a carry-less multiplication by all
            // ones, as on x86 with pclmulqdq.
            #[cfg(target_feature = "aes")]
            let prefix = unsafe { vmull_p64(quote_bits, !0u64) as u64 };
            #[cfg(not(target_feature = "aes"))]
            let prefix = {
                let mut res = quote_bits;
                res ^= res << 1;
                res ^= res << 2;
                res ^= res << 4;
                res ^= res << 8;
                res ^= res << 16;
                res ^= res << 32;
                res
            };
            let quote_mask = prefix ^ *prev_iter_inside_quote;
            *prev_iter_inside_quote = (quote_mask as i64).wrapping_shr(63) as u64;
            (quote_mask, quote_bits)
        }

        // tbl returns 0 for indexes >= 16, so the index is masked to the low nibble first.
        #[inline(always)]
        unsafe fn whitespace_and_newline_masks(self) -> (u64, u64) {
            let [a, b, c, d] = self.0;
            unsafe {
                let table = vld1q_u8(WS_BY_LOW_NIBBLE.as_ptr());
                let low = vdupq_n_u8(0x0F);
                let ws = |v| vceqq_u8(vqtbl1q_u8(table, vandq_u8(v, low)), v);
                let ws = Impl([ws(a), ws(b), ws(c), ws(d)]);
                (ws.mask(), self.cmp_mask_against_input(b'\n'))
            }
        }
    }
}

pub struct ByteReader<P: ChunkProducer> {
    prod: P,
    cur_chunk: P::Chunk,
    cur_buf: Buf,
    buf_len: usize,
    used_fields: FieldSet,
    // Progress in the current buffer.
    progress: usize,
    record_sep: u8,

    last_len: usize,
    check_utf8: bool,
    // The (unused) fields of records read by `read_record`, kept to reuse the allocation.
    scratch: Vec<Str<'static>>,
    // Shared with the chunk producers, see `chunk::LinesOnly`.
    lines_only: chunk::LinesOnly,
}

impl ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk>>> {
    pub fn new<I, S>(
        rs: I,
        field_sep: u8,
        record_sep: u8,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + 'static + Send,
        S: Read + Send + 'static,
    {
        Self::new_internal(
            rs,
            field_sep,
            record_sep,
            chunk_size,
            check_utf8,
            exec_strategy,
            get_find_indexes_bytes(),
            cancel_signal,
        )
    }

    // Not great, but grouping into a separate type is a bit awkward given the
    // different permutations used between these modules.
    //
    // TODO: there are absolutely opportunities here to clean things up.
    #[allow(clippy::too_many_arguments)]
    pub fn new_internal<I, S>(
        rs: I,
        field_sep: u8,
        record_sep: u8,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        kernel: BytesIndexKernel,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + 'static + Send,
        S: Read + Send + 'static,
    {
        let lines_only = chunk::LinesOnly::default();
        let lo = lines_only.clone();
        let prod: Box<dyn ChunkProducer<Chunk = OffsetChunk>> = match exec_strategy {
            ExecutionStrategy::Serial => Box::new(chunk::new_chained_offset_chunk_producer_bytes(
                rs, chunk_size, field_sep, record_sep, check_utf8, kernel, lo,
            )),
            x @ ExecutionStrategy::ShardPerRecord(_) => {
                Box::new(CancellableChunkProducer::new(
                    cancel_signal,
                    ParallelChunkProducer::new(
                        move || {
                            chunk::new_chained_offset_chunk_producer_bytes(
                                rs, chunk_size, field_sep, record_sep, check_utf8, kernel, lo,
                            )
                        },
                        /*channel_size*/ x.num_workers() * 2,
                    ),
                ))
            }
            ExecutionStrategy::ShardPerFile => {
                let iter = rs.enumerate().map(move |(i, (r, name))| {
                    let lo = lo.clone();
                    move || {
                        chunk::new_offset_chunk_producer_bytes(
                            r,
                            chunk_size,
                            name.as_str(),
                            field_sep,
                            record_sep,
                            i as u32 + 1,
                            check_utf8,
                            kernel,
                            lo,
                        )
                    }
                });
                Box::new(CancellableChunkProducer::new(
                    cancel_signal,
                    ShardedChunkProducer::new(iter),
                ))
            }
        };
        ByteReader {
            prod,
            cur_chunk: Default::default(),
            cur_buf: UniqueBuf::new(0).into_buf(),
            buf_len: 0,
            progress: 0,
            record_sep,
            used_fields: FieldSet::all(),
            last_len: usize::MAX,
            check_utf8,
            scratch: Vec::new(),
            lines_only,
        }
    }
}

impl ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk<WhitespaceOffsets>>>> {
    pub fn new_whitespace<I, S>(
        rs: I,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + 'static + Send,
        S: Read + Send + 'static,
    {
        Self::new_whitespace_internal(
            rs,
            chunk_size,
            check_utf8,
            exec_strategy,
            get_find_indexes_ascii_whitespace(),
            cancel_signal,
        )
    }
    pub fn new_whitespace_internal<I, S>(
        rs: I,
        chunk_size: usize,
        check_utf8: bool,
        exec_strategy: ExecutionStrategy,
        find_indexes: unsafe fn(&[u8], &mut WhitespaceOffsets, u64) -> u64,
        cancel_signal: CancelSignal,
    ) -> Self
    where
        I: Iterator<Item = (S, String)> + 'static + Send,
        S: Read + Send + 'static,
    {
        let lines_only = chunk::LinesOnly::default();
        let lo = lines_only.clone();
        let prod: Box<dyn ChunkProducer<Chunk = OffsetChunk<WhitespaceOffsets>>> =
            match exec_strategy {
                ExecutionStrategy::Serial => {
                    Box::new(chunk::new_chained_offset_chunk_producer_ascii_whitespace(
                        rs,
                        chunk_size,
                        check_utf8,
                        find_indexes,
                        lo,
                    ))
                }
                x @ ExecutionStrategy::ShardPerRecord(_) => {
                    Box::new(CancellableChunkProducer::new(
                        cancel_signal,
                        ParallelChunkProducer::new(
                            move || {
                                chunk::new_chained_offset_chunk_producer_ascii_whitespace(
                                    rs,
                                    chunk_size,
                                    check_utf8,
                                    find_indexes,
                                    lo,
                                )
                            },
                            /*channel_size*/ x.num_workers() * 2,
                        ),
                    ))
                }
                ExecutionStrategy::ShardPerFile => {
                    let iter = rs.enumerate().map(move |(i, (r, name))| {
                        let lo = lo.clone();
                        move || {
                            chunk::new_offset_chunk_producer_ascii_whitespace(
                                r,
                                chunk_size,
                                name.as_str(),
                                i as u32 + 1,
                                check_utf8,
                                find_indexes,
                                lo,
                            )
                        }
                    });
                    Box::new(CancellableChunkProducer::new(
                        cancel_signal,
                        ShardedChunkProducer::new(iter),
                    ))
                }
            };
        ByteReader {
            prod,
            cur_chunk: Default::default(),
            cur_buf: UniqueBuf::new(0).into_buf(),
            buf_len: 0,
            progress: 0,
            record_sep: 0, // unused
            used_fields: FieldSet::all(),
            last_len: usize::MAX,
            check_utf8,
            scratch: Vec::new(),
            lines_only,
        }
    }
}

impl<C: Chunk + 'static> LineReader for ByteReader<Box<dyn ChunkProducer<Chunk = C>>>
where
    Self: ByteReaderBase,
{
    type Line = DefaultLine;
    fn filename(&self) -> Str<'static> {
        Str::from(self.cur_chunk.get_name()).unmoor()
    }
    fn file_version(&self) -> u32 {
        self.cur_chunk_version()
    }
    fn check_utf8(&self) -> bool {
        self.check_utf8
    }
    fn wait(&self) -> bool {
        ByteReaderBase::wait(self)
    }
    fn request_handles(&self, size: usize) -> Vec<Box<dyn FnOnce() -> Self + Send>> {
        let producers = self.prod.try_dyn_resize(size);
        let mut res = Vec::with_capacity(producers.len());
        for p_factory in producers.into_iter() {
            let used_fields = self.used_fields.clone();
            let record_sep = self.record_sep;
            let check_utf8 = self.check_utf8;
            let lines_only = self.lines_only.clone();
            res.push(Box::new(move || ByteReader {
                prod: p_factory(),
                cur_chunk: Default::default(),
                cur_buf: UniqueBuf::new(0).into_buf(),
                buf_len: 0,
                progress: 0,
                record_sep,
                last_len: usize::MAX,
                used_fields,
                check_utf8,
                scratch: Vec::new(),
                lines_only,
            }) as _)
        }
        res
    }
    fn read_line(&mut self, _pat: &Str, _rc: &mut RegexCache) -> Result<(bool, DefaultLine)> {
        let mut line = DefaultLine::default();
        let changed = self.read_line_reuse(_pat, _rc, &mut line)?;
        Ok((changed, line))
    }
    fn read_line_reuse<'a, 'b: 'a>(
        &'b mut self,
        _pat: &Str,
        _rc: &mut RegexCache,
        old: &'a mut DefaultLine,
    ) -> Result<bool> {
        let start = self.cur_chunk_version() == 0;
        old.diverged = false;
        old.assigned = false;
        // We use the same protocol as DefaultSplitter, RegexSplitter. See comments for more info.
        if start {
            old.used_fields = self.used_fields.clone();
        } else if old.used_fields != self.used_fields {
            self.apply_used_fields(old.used_fields.clone())
        }
        old.fields.clear();
        let changed = self.read_line_inner(&mut old.line, &mut old.fields)?;
        Ok(changed)
    }
    fn read_record(&mut self, _pat: &Str, _rc: &mut RegexCache) -> Result<(bool, Str<'static>)> {
        // Only $0 is wanted: don't slice out the fields.
        let used_fields = mem::replace(&mut self.used_fields, FieldSet::singleton(0));
        let mut fields = mem::take(&mut self.scratch);
        let mut line = Str::default();
        let res = self.read_line_inner(&mut line, &mut fields);
        fields.clear();
        self.scratch = fields;
        self.used_fields = used_fields;
        Ok((res?, line))
    }
    fn read_state(&self) -> i64 {
        if self.cur_chunk_version() != 0 && self.last_len == 0 {
            ReaderState::Eof as i64
        } else {
            ReaderState::OK as i64
        }
    }

    fn next_file(&mut self) -> Result<bool> {
        self.cur_chunk = C::default();
        self.cur_buf = UniqueBuf::new(0).into_buf();
        self.buf_len = 0;
        self.progress = 0;
        self.prod.next_file()
    }

    fn set_used_fields(&mut self, field_set: &FieldSet) {
        self.apply_used_fields(field_set.clone());
    }
}

impl<P: ChunkProducer> ByteReader<P> {
    fn apply_used_fields(&mut self, used_fields: FieldSet) {
        // Without fields or NF to compute, only the record boundaries have to be found.
        self.lines_only.store(
            used_fields.record_only(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.used_fields = used_fields;
    }
}

// Most of the implementation for splitting by whitespace and splitting by a single byte is
// shared. ByteReaderBase encapsulates the portions of the implementation that are different. This
// isn't the cleanest abstraction, but given that it doesn't tend to "leak" into the rest of the
// code-base, the win in code deduplication seems to justify its existence.
pub(crate) trait ByteReaderBase {
    fn read_line_inner<'a, 'b: 'a>(
        &'b mut self,
        line: &'a mut Str<'static>,
        fields: &'a mut Vec<Str<'static>>,
    ) -> Result</*file changed*/ bool>;

    fn maybe_done(&self) -> bool;
    fn refresh_buf(&mut self) -> Result<(/*eof*/ bool, /*file changed*/ bool)>;
    unsafe fn consume_line<'a, 'b: 'a>(
        &'b mut self,
        fields: &'a mut Vec<Str<'static>>,
    ) -> (Str<'static>, /*bytes consumed*/ usize);
    fn cur_chunk_version(&self) -> u32;
    fn wait(&self) -> bool;
}

fn refresh_buf_impl<T, P: ChunkProducer<Chunk = OffsetChunk<T>>>(
    br: &mut ByteReader<P>,
) -> Result<(bool, bool)>
where
    OffsetChunk<T>: Chunk,
{
    let prev_version = br.cur_chunk.version;
    if br.prod.get_chunk(&mut br.cur_chunk)? {
        // See comment in the equivalent line in CSVReader.
        br.cur_chunk.version = std::cmp::max(prev_version, 1);
        return Ok((true, false));
    }
    br.cur_buf = br.cur_chunk.buf.take().unwrap().into_buf();
    br.buf_len = br.cur_chunk.len;
    br.progress = 0;
    Ok((false, prev_version != br.cur_chunk.version))
}

fn read_line_inner_impl<'a, 'b: 'a, T, P: ChunkProducer<Chunk = OffsetChunk<T>>>(
    br: &'b mut ByteReader<P>,
    line: &'a mut Str<'static>,
    fields: &'a mut Vec<Str<'static>>,
) -> Result<bool>
where
    OffsetChunk<T>: Chunk,
    ByteReader<P>: ByteReaderBase,
{
    let mut changed = false;
    if br.maybe_done() {
        // What's going on with this second test? br.refresh_buf() returns Ok(true) if we
        // were unable to fetch more data due to an EOF. The last execution consumed buffer up
        // to the last record separator in the input, but there may be remaining data filling
        // up the rest of the buffer with no more record or field separators. In that case, we
        // want to return the rest of the input as a single-field record, which one more
        // `consume_line` will indeed accomplish.
        let (is_eof, has_changed) = br.refresh_buf()?;
        changed = has_changed;
        if is_eof && br.progress == br.buf_len {
            *line = Str::default();
            br.last_len = 0;
            debug_assert!(!changed);
            return Ok(false);
        }
    }
    let (next_line, consumed) = unsafe { br.consume_line(fields) };
    *line = next_line;
    br.last_len = consumed;
    Ok(changed)
}

impl<P: ChunkProducer<Chunk = OffsetChunk>> ByteReaderBase for ByteReader<P> {
    fn cur_chunk_version(&self) -> u32 {
        self.cur_chunk.version
    }
    fn refresh_buf(&mut self) -> Result<(bool, bool)> {
        refresh_buf_impl(self)
    }
    fn maybe_done(&self) -> bool {
        // A last record without a record separator (and without a field separator) has no offset
        // left, but is not consumed yet: it would be lost if another file follows.
        self.cur_chunk.off.rel.start == self.cur_chunk.off.rel.fields.len()
            && self.progress >= self.buf_len
    }
    fn wait(&self) -> bool {
        self.prod.wait()
    }
    fn read_line_inner<'a, 'b: 'a>(
        &'b mut self,
        line: &'a mut Str<'static>,
        fields: &'a mut Vec<Str<'static>>,
    ) -> Result<bool> {
        read_line_inner_impl(self, line, fields)
    }
    unsafe fn consume_line<'a, 'b: 'a>(
        &'b mut self,
        fields: &'a mut Vec<Str<'static>>,
    ) -> (Str<'static>, usize) {
        let buf = &self.cur_buf;
        macro_rules! get_field {
            ($fld:expr, $start:expr, $end:expr) => {
                if self.used_fields.get($fld) {
                    buf.slice_to_str($start, $end)
                } else {
                    Str::default()
                }
            };
            ($index:expr) => {
                get_field!(fields.len() + 1, self.progress, $index)
            };
        }

        let line_start = self.progress;
        if self.cur_chunk.off.lines_only {
            // `rel` holds the record separators only. The fields are left empty, so `DefaultLine`
            // splits `$0` if they are used after all: keep `$0` whatever the used fields are.
            let rel = &mut self.cur_chunk.off.rel;
            let end = if rel.start < rel.fields.len() {
                let end = rel.fields[rel.start] as usize;
                rel.start += 1;
                self.progress = end + 1;
                end
            } else {
                self.progress = self.buf_len;
                self.buf_len
            };
            return (
                buf.slice_to_str(line_start, end),
                self.progress - line_start,
            );
        }
        let max = self.used_fields.max_value() as usize;
        let offs = &mut self.cur_chunk.off;
        let end = offs
            .nl
            .fields
            .get(offs.nl.start)
            .map(|x| *x as usize)
            .unwrap_or(self.buf_len);
        for index in &offs.rel.fields[offs.rel.start..] {
            let mut index = *index as usize;
            debug_assert!(
                index < self.buf_len,
                "buf_len={} index={}, off.fields={:?}",
                self.buf_len,
                index,
                &offs.rel.fields[offs.rel.start..]
            );

            offs.rel.start += 1;
            let mut is_record_sep = index == end;
            if !(is_record_sep && fields.is_empty() && self.progress == index) {
                // If we have a field of length 0, NF should be zero. This check fires when
                // record_sep is the first offset we see for this line, and it occurs as the first
                // character in the line.
                fields.push(get_field!(index));
            }
            if fields.len() == max {
                let start_inc = gallop(&offs.rel.fields[offs.rel.start..], |ix| ix as usize <= end);
                // If the record is not terminated by a record separator (i.e. the input ends
                // without a trailing newline), the last field has no separator offset after it,
                // so it has to be counted explicitly.
                let unterminated = end == self.buf_len;
                let len_inc = fields.len() + start_inc + usize::from(unterminated);
                fields.resize_with(len_inc, Str::default);
                offs.rel.start += start_inc;
                if unterminated {
                    offs.nl.start += 1;
                    self.progress = self.buf_len;
                    let line = get_field!(0, line_start, self.buf_len);
                    return (line, self.buf_len - line_start);
                }
                index = end;
                is_record_sep = true;
            }
            self.progress = index + 1;
            if is_record_sep {
                offs.nl.start += 1;
                let line = get_field!(0, line_start, index);
                return (line, self.progress - line_start);
            }
        }
        offs.nl.start += 1;
        fields.push(get_field!(self.buf_len));
        self.progress = self.buf_len;
        let line = get_field!(0, line_start, self.buf_len);
        (line, self.buf_len - line_start)
    }
}

impl ByteReaderBase for ByteReader<Box<dyn ChunkProducer<Chunk = OffsetChunk<WhitespaceOffsets>>>> {
    fn cur_chunk_version(&self) -> u32 {
        self.cur_chunk.version
    }
    fn wait(&self) -> bool {
        self.prod.wait()
    }
    fn refresh_buf(&mut self) -> Result<(bool, bool)> {
        refresh_buf_impl(self)
    }
    fn maybe_done(&self) -> bool {
        if self.cur_chunk.off.0.lines_only {
            // A last record without a newline has no offset left, but is not consumed yet.
            return self.progress >= self.buf_len;
        }
        // TODO: This is a pretty gross check; we should see if we can streamline the ws.fields check.
        self.cur_chunk.off.0.nl.start == self.cur_chunk.off.0.nl.fields.len()
            && (self.cur_chunk.off.0.rel.start == self.cur_chunk.off.0.rel.fields.len()
                || self.progress == self.buf_len)
    }
    fn read_line_inner<'a, 'b: 'a>(
        &'b mut self,
        line: &'a mut Str<'static>,
        fields: &'a mut Vec<Str<'static>>,
    ) -> Result<bool> {
        read_line_inner_impl(self, line, fields)
    }
    unsafe fn consume_line<'a, 'b: 'a>(
        &'b mut self,
        fields: &'a mut Vec<Str<'static>>,
    ) -> (Str<'static>, usize) {
        let buf = &self.cur_buf;
        macro_rules! get_field {
            ($fld:expr, $start:expr, $end:expr) => {
                if self.used_fields.get($fld) {
                    buf.slice_to_str($start, $end)
                } else {
                    Str::default()
                }
            };
            ($index:expr) => {
                get_field!(fields.len() + 1, self.progress, $index)
            };
        }
        let line_start = self.progress;
        let offs_nl = &mut self.cur_chunk.off.0.nl;
        let record_end = if offs_nl.start == offs_nl.fields.len() {
            self.buf_len
        } else {
            let record_end = offs_nl.fields[offs_nl.start];
            offs_nl.start += 1;
            record_end as usize
        };

        // If we are at the very end of the buffer, consume no bytes and finish early. Running this
        // normally would yield semantics not unlike split by newline in normal rust code, but it
        // would result in us yielding an extra empty record in inputs that end in a newline when
        // compared with awk, so we finish early instead.
        if line_start == self.buf_len {
            return (Str::default(), 0);
        }

        // See the comments for Vector::whitespace_masks for more info on the format of the offsets
        // here.
        //
        // The goal is to parse the available whitespace runs that appear before the next newline.
        // If a field is ended by the newline, it will appear in ws.fields, so the only case in
        // which we will not have an even number of offsets is when we are at the end of the entire
        // input stream. That leaves us two cases:
        //
        // 1. We have a pair of start and end offsets for each field, these are appended to the
        //    `fields` vector if they're present in used_fields.
        // 2. We are at the end of the input, in which case we take from the start offset to the
        //    end of the buffer.
        let mut iter = self.cur_chunk.off.0.rel.fields[self.cur_chunk.off.0.rel.start..]
            .iter()
            .cloned()
            .map(|x| x as usize)
            .take_while(|x| x <= &record_end);
        while let Some(field_start) = iter.next() {
            self.progress = field_start;
            self.cur_chunk.off.0.rel.start += 1;
            if let Some(field_end) = iter.next() {
                fields.push(get_field!(field_end));
                self.progress = field_end + 1;
                self.cur_chunk.off.0.rel.start += 1;
            } else if self.progress != record_end {
                fields.push(get_field!(record_end));
            }
        }
        self.progress = record_end + 1;
        let consumed = self.progress - line_start;
        if line_start < record_end && self.cur_chunk.off.0.lines_only {
            // No field offsets: `DefaultLine` splits `$0` if fields are used after all.
            (buf.slice_to_str(line_start, record_end), consumed)
        } else if line_start < record_end {
            (get_field!(0, line_start, record_end), consumed)
        } else {
            (Str::default(), consumed)
        }
    }
}

// adapted from Frank McSherry:
// https://github.com/frankmcsherry/blog/blob/master/posts/2018-05-19.md

fn gallop(slice: &[u32], mut cmp: impl FnMut(u32) -> bool) -> usize {
    let mut res = 0;
    // if empty slice, or already >= element, return
    if slice.len() > res && cmp(slice[res]) {
        let mut step = 1;
        while res + step < slice.len() && cmp(slice[res + step]) {
            res += step;
            step <<= 1;
        }

        step >>= 1;
        while step > 0 {
            if step + res < slice.len() && cmp(slice[res + step]) {
                res += step;
            }
            step >>= 1;
        }
        res += 1; // advance one, as we always stayed < value
    }

    // TODO: experiment with doing just one round of the doubling/halving, and then doing a linear
    // search thereafter. Most rows where this matters will have a pretty small number of columns

    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::iter;

    #[test]
    fn escape_csv_quotes_only_when_needed() {
        let esc = |t: &str| escape_csv(&Str::from(t)).as_str().into_owned();
        assert_eq!(esc("plain"), "plain");
        assert_eq!(esc(""), "");
        assert_eq!(esc("a,b"), "\"a,b\"");
        assert_eq!(esc("a\rb"), "\"a\rb\"");
        assert_eq!(esc(" lead"), "\" lead\"");
        assert_eq!(esc("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(esc("\"\"\"\"\"\"\"\"\"\""), format!("\"{}\"", "\"".repeat(20)));
    }

    fn smoke_test<V: generic::Vector>() {
        let text: &'static str = r#"This,is,"a line with a quoted, comma",and
unquoted,commas,"as well, including some long ones", and there we have it.""#;
        let mut mem: Vec<u8> = text.as_bytes().to_vec();
        mem.reserve(32);
        let mut offsets: Offsets = Default::default();
        let (in_quote, in_cr) =
            unsafe { generic::find_indexes_csv::<V>(&mem[..], &mut offsets, 0, 0) };
        // The final `"` is irregular, so the last record is scanned by the lenient scan, which
        // does not track the quote state by parity.
        assert_eq!(in_quote, 0);
        assert_eq!(in_cr, 0);
        // The final `"` follows `it.`, so it is a literal quote and not an offset.
        assert_eq!(
            &offsets.rel.fields[..],
            &[4, 7, 8, 36, 37, 41, 50, 57, 58, 92, 93],
            "offset_fields={:?}",
            offsets
                .rel
                .fields
                .iter()
                .cloned()
                .map(|x| (x, mem[x as usize] as char))
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn csv_smoke_test() {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if is_x86_feature_detected!("avx2") {
                    smoke_test::<avx2::Impl>();
                }
                if is_x86_feature_detected!("sse2") {
                    smoke_test::<sse2::Impl>();
                }
            } else if #[cfg(target_arch = "aarch64")] {
                smoke_test::<neon::Impl>();
            }
        }
        smoke_test::<generic::Impl>();
    }

    #[test]
    fn generic_quote_mask_matches_naive() {
        use generic::Vector;
        // xorshift64: deterministic inputs, about a third of the bytes are quotes.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let mut buf = [0u8; generic::Impl::INPUT_SIZE];
            for b in buf.iter_mut() {
                *b = if next() % 3 == 0 { b'"' } else { b'a' };
            }
            let prev = if next() & 1 == 0 { 0 } else { !0u64 };
            let mut want_prev = prev;
            let quote_mask = buf
                .iter()
                .enumerate()
                .fold(0u64, |m, (i, b)| m | ((*b == b'"') as u64) << i);
            let mut running_xor = 0;
            let mut naive = 0u64;
            for ix in 0..64 {
                running_xor ^= quote_mask.wrapping_shr(ix) & 1;
                naive |= running_xor.wrapping_shl(ix);
            }
            naive ^= want_prev;
            want_prev = (naive as i64).wrapping_shr(63) as u64;

            let mut got_prev = prev;
            let (in_quotes, quotes) =
                unsafe { generic::Impl::fill_input(buf.as_ptr()).find_quote_mask(&mut got_prev) };
            assert_eq!(quotes, quote_mask);
            assert_eq!(in_quotes, naive, "buf={:?} prev={prev:#x}", buf);
            assert_eq!(got_prev, want_prev);
        }
    }

    /// The vector's whitespace and newline masks agree with a byte-by-byte check, over random
    /// inputs drawn from all 256 byte values and from a whitespace-heavy alphabet.
    fn whitespace_masks_match_naive<V: generic::Vector>() {
        let mut seed = 0x0123_4567_89AB_CDEFu64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        const ALPHABET: &[u8] = b" \t\n\r\0)*-a\x80\x8a\xa0\xff\x0b\x0c";
        let mut buf = [0u8; 64];
        // Every byte value at least once in each position class.
        for start in (0..256).step_by(V::INPUT_SIZE) {
            for (i, b) in buf.iter_mut().enumerate().take(V::INPUT_SIZE) {
                *b = ((start + i) % 256) as u8;
            }
            check_whitespace_masks::<V>(&buf);
        }
        for round in 0..20_000 {
            for b in buf.iter_mut() {
                let r = next();
                *b = if round % 2 == 0 {
                    r as u8
                } else {
                    ALPHABET[(r % ALPHABET.len() as u64) as usize]
                };
            }
            check_whitespace_masks::<V>(&buf);
        }
    }

    fn check_whitespace_masks<V: generic::Vector>(buf: &[u8; 64]) {
        let naive = |p: fn(u8) -> bool| {
            buf[..V::INPUT_SIZE]
                .iter()
                .enumerate()
                .fold(0u64, |acc, (i, b)| acc | (p(*b) as u64) << i)
        };
        let want_ws = naive(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        let want_nl = naive(|b| b == b'\n');
        let v = unsafe { V::fill_input(buf.as_ptr()) };
        let (ws, nl) = unsafe { v.whitespace_and_newline_masks() };
        assert_eq!(ws, want_ws, "buf={:?}", &buf[..V::INPUT_SIZE]);
        assert_eq!(nl, want_nl, "buf={:?}", &buf[..V::INPUT_SIZE]);
    }

    #[test]
    fn whitespace_masks_match_naive_all_impls() {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if is_x86_feature_detected!("avx2") {
                    whitespace_masks_match_naive::<avx2::Impl>();
                }
                if is_x86_feature_detected!("ssse3") {
                    whitespace_masks_match_naive::<sse2::Impl<true>>();
                }
                whitespace_masks_match_naive::<sse2::Impl>();
            } else if #[cfg(target_arch = "aarch64")] {
                whitespace_masks_match_naive::<neon::Impl>();
            }
        }
        whitespace_masks_match_naive::<generic::Impl>();
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_masks_match_naive() {
        use generic::Vector;
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        const ALPHABET: &[u8] = b"a,\"\t \n\r;";
        for _ in 0..20_000 {
            let mut buf = [0u8; 64];
            for b in buf.iter_mut() {
                *b = ALPHABET[(next() % ALPHABET.len() as u64) as usize];
            }
            let naive_mask = |m: u8| {
                buf.iter()
                    .enumerate()
                    .fold(0u64, |acc, (i, b)| acc | ((*b == m) as u64) << i)
            };
            let v = unsafe { neon::Impl::fill_input(buf.as_ptr()) };
            for m in [b',', b'"', b'\t', b'\n', b'a'] {
                assert_eq!(unsafe { v.cmp_mask_against_input(m) }, naive_mask(m));
            }
            let either = unsafe {
                v.cmp_against_input(b',')
                    .or(v.cmp_against_input(b'a'))
                    .mask()
            };
            assert_eq!(either, naive_mask(b',') | naive_mask(b'a'));

            let quotes = naive_mask(b'"');
            let mut parity = 0u64;
            let mut naive = 0u64;
            for ix in 0..64 {
                parity ^= (quotes >> ix) & 1;
                naive |= parity << ix;
            }
            let prev = if next() & 1 == 0 { 0 } else { !0u64 };
            naive ^= prev;
            let mut got_prev = prev;
            let (in_quotes, got_quotes) = unsafe { v.find_quote_mask(&mut got_prev) };
            assert_eq!(got_quotes, quotes);
            assert_eq!(in_quotes, naive);
            assert_eq!(got_prev, (naive as i64 >> 63) as u64);
        }
    }

    fn csv_read_all(corpus: &str, chunk_size: usize) -> Vec<Vec<String>> {
        let mut _cache = RegexCache::default();
        let _pat = Str::default();
        let mut reader = CSVReader::new(
            iter::once((
                std::io::Cursor::new(corpus.to_string()),
                String::from("fake-stdin"),
            )),
            InputFormat::CSV,
            chunk_size,
            /*check_utf8=*/ true,
            ExecutionStrategy::Serial,
            Default::default(),
        );
        let mut got = Vec::new();
        loop {
            let (_, line) = reader
                .read_line(&_pat, &mut _cache)
                .expect("failed to read line");
            if reader.read_state() != 1 {
                break;
            }
            got.push(line.fields.iter().map(|s| s.to_string()).collect());
        }
        got
    }

    #[test]
    fn csv_blank_lines_have_no_fields() {
        let got = csv_read_all("a,b\n\n\r\n\"\"\n,\nc,d\n", 512);
        let expected: &[&[&str]] = &[&["a", "b"], &[], &[], &[""], &["", ""], &["c", "d"]];
        assert_eq!(got, expected);
    }

    #[test]
    fn csv_quotes_inside_fields() {
        // Like gawk: only a quote at the start of a field opens a quoted field, and a quote inside
        // one that is not followed by a quote, a separator or a line end is literal.
        let cases: &[(&str, &[&[&str]])] = &[
            ("ab\"c,d\n", &[&["ab\"c", "d"]]),
            ("ab\"c,d\nx,y\n", &[&["ab\"c", "d"], &["x", "y"]]),
            ("a\"\"b,c\n", &[&["a\"\"b", "c"]]),
            ("\"ab\"cd\",e\n", &[&["ab\"cd", "e"]]),
            ("\"{\\\"a\\\":1}\",x\n", &[&["{\\\"a\\\":1}", "x"]]),
            (
                "\"a\"\"b\",\"\"\r\nx\"y,\"z\"\n",
                &[&["a\"b", ""], &["x\"y", "z"]],
            ),
            ("\"a\nb\",c\"\nd,\"e\"", &[&["a\nb", "c\""], &["d", "e"]]),
            // After a literal quote, a newline outside quotes (by parity) ends the record.
            (
                "\"a\"b,c\nnext,line\nthird,x\n",
                &[&["a\"b,c"], &["next", "line"], &["third", "x"]],
            ),
            ("\"a\"b\r\nx,y\n", &[&["a\"b"], &["x", "y"]]),
            (
                "\"a\"b \"x\ny\" z\nq,r\n",
                &[&["a\"b \"x\ny\" z"], &["q", "r"]],
            ),
            (
                "\"a\"b,\"c\",d\r\nlast,1\n",
                &[&["a\"b,\"c", "d"], &["last", "1"]],
            ),
        ];
        for (input, expected) in cases {
            let expected: Vec<Vec<String>> = expected
                .iter()
                .map(|l| l.iter().map(|s| s.to_string()).collect())
                .collect();
            assert_eq!(csv_read_all(input, 512), expected, "input: {:?}", input);
        }

        // Literal quotes spread over many chunks, mixed with regular quoted fields.
        let line = "id\"1,\"a, \"\"b\"\"\",\"x\"y\",z\"\n";
        let corpus = line.repeat(500);
        let got = csv_read_all(&corpus, 1024);
        assert_eq!(got.len(), 500);
        for fields in got {
            assert_eq!(fields, ["id\"1", "a, \"b\"", "x\"y", "z\""]);
        }

        // A malformed record repeated across chunk boundaries does not swallow what follows.
        let corpus = "\"a\"b,c\nx,y\n".repeat(500);
        let got = csv_read_all(&corpus, 1024);
        assert_eq!(got.len(), 1000);
        for pair in got.chunks(2) {
            assert_eq!(
                pair,
                [
                    vec!["a\"b,c".to_string()],
                    vec!["x".to_string(), "y".to_string()]
                ]
            );
        }
    }

    fn disp_vec(v: &[Str]) -> String {
        format!(
            "{:?}",
            v.iter().map(|s| format!("{}", s)).collect::<Vec<String>>()
        )
    }

    fn tsv_split(corpus: &str) {
        // Replace spaces with tabs to get some traction here.
        let fs = b'\t';
        let rs = b'\n';
        let corpus = String::from(corpus).replace(' ', "\t");
        let mut _cache = RegexCache::default();
        let _pat = Str::default();
        let expected: Vec<Vec<Str<'static>>> = corpus
            .split(rs as char)
            .map(|line| {
                // Blank lines have no fields.
                if line.is_empty() {
                    return Vec::new();
                }
                line.split(fs as char)
                    .map(|x| Str::from(x).unmoor())
                    .collect()
            })
            .collect();
        let mut got = Vec::with_capacity(corpus.len());
        let reader = std::io::Cursor::new(corpus);
        let mut reader = CSVReader::new(
            iter::once((reader, String::from("fake-stdin"))),
            InputFormat::TSV,
            /*chunk_size=*/ 512,
            /*check_utf8=*/ true,
            ExecutionStrategy::Serial,
            Default::default(),
        );
        loop {
            let (_, line) = reader
                .read_line(&_pat, &mut _cache)
                .expect("failed to read line");
            if reader.read_state() != 1 {
                break;
            }
            got.push(line.fields.clone());
        }
        if got != expected {
            eprintln!(
                "test failed! got vector of length {}, expected {} lines",
                got.len(),
                expected.len()
            );
            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                eprintln!("===============");
                if g == e {
                    eprintln!("line {} matches", i);
                } else {
                    eprintln!("line {} has a mismatch", i);
                    eprintln!("got:  {}", disp_vec(g));
                    eprintln!("want: {}", disp_vec(e));
                }
            }
            panic!("test failed. See debug output");
        }
    }

    #[test]
    fn tsv_splitter_basic() {
        tsv_split(crate::test_string_constants::VIRGIL);
        tsv_split(crate::test_string_constants::PRIDE_PREJUDICE_CH2);
    }

    /// Read every record of `files` (as separate inputs) with the given used fields, and return
    /// each `$0` with its NF, as computed through `DefaultLine` (which splits lazily).
    fn read_records<LR: LineReader<Line = DefaultLine>>(
        mut reader: LR,
        used: FieldSet,
        fs: &str,
    ) -> Vec<(String, usize)> {
        use crate::runtime::splitter::Line;
        let mut cache = RegexCache::default();
        let pat = Str::from(fs).unmoor();
        reader.set_used_fields(&used);
        let mut res = Vec::new();
        loop {
            let (_, mut line) = reader.read_line(&pat, &mut cache).unwrap();
            if reader.read_state() != 1 {
                break;
            }
            let nf = line.nf(&pat, &mut cache).unwrap();
            res.push((line.line.to_string(), nf));
        }
        res
    }

    #[test]
    fn unterminated_record_before_next_file() {
        // The first file ends with a record that has neither a record separator nor a field
        // separator; it used to be dropped when another file followed.
        let files = || {
            [("a b\nc d e", "f1"), ("x,y\nlast", "f2"), ("z,1\n", "f3")]
                .into_iter()
                .map(|(s, n)| (io::Cursor::new(s), String::from(n)))
        };
        let want_comma: Vec<(String, usize)> = [
            ("a b", 1),
            ("c d e", 1),
            ("x,y", 2),
            ("last", 1),
            ("z,1", 2),
        ]
        .iter()
        .map(|(l, n)| (l.to_string(), *n))
        .collect();
        let want_ws: Vec<(String, usize)> = [
            ("a b", 2),
            ("c d e", 3),
            ("x,y", 1),
            ("last", 1),
            ("z,1", 1),
        ]
        .iter()
        .map(|(l, n)| (l.to_string(), *n))
        .collect();
        // All fields, only NF (fields counted by the kernel), and only $0 (lines only: NF comes
        // from splitting $0 lazily).
        let mut nf_only = FieldSet::singleton(0);
        nf_only.union(&FieldSet::nf());
        for used in [
            FieldSet::all(),
            nf_only,
            FieldSet::singleton(0),
            FieldSet::empty(),
        ] {
            for strategy in [ExecutionStrategy::Serial, ExecutionStrategy::ShardPerFile] {
                let br = ByteReader::new(
                    files(),
                    b',',
                    b'\n',
                    1024,
                    true,
                    strategy,
                    CancelSignal::default(),
                );
                assert_eq!(
                    read_records(br, used.clone(), ","),
                    want_comma,
                    "used={used:?}"
                );
                let br = ByteReader::new_whitespace(
                    files(),
                    1024,
                    true,
                    strategy,
                    CancelSignal::default(),
                );
                assert_eq!(
                    read_records(br, used.clone(), " "),
                    want_ws,
                    "used={used:?}"
                );
            }
        }
    }

    #[test]
    fn lines_only_across_chunks() {
        // Small chunks, so that records span buffers and the buffer grows.
        let corpus: String = (0..2000)
            .map(|i| format!("{i} {} x{}\n", "w ".repeat(i % 7), i % 5))
            .collect();
        let corpus: &'static str = Box::leak(corpus.into_boxed_str());
        let want: Vec<(String, usize)> = corpus
            .lines()
            .map(|l| (l.to_string(), l.split_whitespace().count()))
            .collect();
        for used in [FieldSet::all(), FieldSet::singleton(0), FieldSet::empty()] {
            let br = ByteReader::new_whitespace(
                iter::once((io::Cursor::new(corpus), String::from("-"))),
                64,
                true,
                ExecutionStrategy::Serial,
                CancelSignal::default(),
            );
            assert_eq!(read_records(br, used.clone(), " "), want);
            let br = ByteReader::new(
                iter::once((io::Cursor::new(corpus), String::from("-"))),
                b'w',
                b'\n',
                64,
                true,
                ExecutionStrategy::Serial,
                CancelSignal::default(),
            );
            let want_bytes: Vec<(String, usize)> = corpus
                .lines()
                .map(|l| (l.to_string(), l.split('w').count()))
                .collect();
            assert_eq!(read_records(br, used, "w"), want_bytes);
        }
    }

    /// Scan `buf[..n]` after a scan of `buf[..p]`, continuing from its checkpoint, and check that
    /// the offsets and the returned state are the same as for a fresh scan of `buf[..n]`.
    fn check_resumed_scan<S: PartialEq + std::fmt::Debug>(
        buf: &[u8],
        p: usize,
        mut scan: impl FnMut(&[u8], &mut Offsets) -> S,
    ) {
        let mut fresh = Offsets::default();
        let want = scan(buf, &mut fresh);
        let mut off = Offsets::default();
        scan(&buf[..p], &mut off);
        off.continue_scan();
        let got = scan(buf, &mut off);
        assert_eq!(
            got,
            want,
            "state; p={p} buf={:?}",
            String::from_utf8_lossy(buf)
        );
        assert_eq!(off.rel.fields, fresh.rel.fields, "rel; p={p}");
        assert_eq!(off.nl.fields, fresh.nl.fields, "nl; p={p}");
        // Without continue_scan, the next scan starts over.
        let got = scan(buf, &mut off);
        assert_eq!(got, want);
        assert_eq!(off.rel.fields, fresh.rel.fields);
    }

    fn resumed_scan_matches_full_scan<V: generic::Vector>() {
        let mut seed = 0x51_7C_C1_B7_27_22_0A_95u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        // Quotes are rare, so that most buffers have regular quotes and take the SIMD path.
        const ALPHABET: &[u8] = b"aaaabbbcc  \t\t,,,,\n\r\"\\w";
        for _ in 0..400 {
            let n = (next() % 700) as usize;
            let mut buf: Vec<u8> = (0..n)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            if next() % 2 == 0 {
                buf.retain(|b| *b != b'"');
            }
            let p = (next() as usize) % (buf.len() + 1);
            check_resumed_scan(&buf, p, |b, off| unsafe {
                generic::find_indexes_csv::<V>(b, off, 0, 0)
            });
            check_resumed_scan(&buf, p, |b, off| unsafe {
                generic::find_indexes_tsv::<V>(b, off, 0, 0)
            });
            check_resumed_scan(&buf, p, |b, off| unsafe {
                generic::find_indexes_byte::<V>(b, off, b'w', b'\n')
            });
            check_resumed_scan(&buf, p, |b, off| unsafe {
                let mut ws = WhitespaceOffsets(mem::take(off));
                let next_ws = generic::find_indexes_ascii_whitespace::<V>(b, &mut ws, 1);
                *off = ws.0;
                next_ws
            });
        }
    }

    #[test]
    fn resumed_scan_matches_full_scan_generic() {
        resumed_scan_matches_full_scan::<generic::Impl>();
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn resumed_scan_matches_full_scan_neon() {
        resumed_scan_matches_full_scan::<neon::Impl>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn resumed_scan_matches_full_scan_sse2() {
        if is_x86_feature_detected!("sse2") && is_x86_feature_detected!("pclmulqdq") {
            resumed_scan_matches_full_scan::<sse2::Impl>();
        }
        if is_x86_feature_detected!("ssse3") && is_x86_feature_detected!("pclmulqdq") {
            resumed_scan_matches_full_scan::<sse2::Impl<true>>();
        }
    }

    /// The SIMD CSV scan, which only redoes the records around irregular quotes byte by byte,
    /// finds the same offsets as the lenient scan of the whole buffer, also when continued.
    fn csv_irregular_quotes_match_lenient<V: generic::Vector>() {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        const WORDS: &[&[u8]] = &[
            b"abc",
            b"",
            b"\"q,u\noted\"",
            b"\"esc\"\"aped\"",
            b"x y",
            b"\"\"",
            b"\"cr\r\nin\"",
        ];
        const DIRT: &[&[u8]] = &[b"\"", b"a\"b", b"\"x\" y", b"\"x\"\ry", b"\r", b"\"open"];
        for round in 0..600 {
            // Mostly well-formed records, with irregular quotes now and then (or often).
            let dirt_rate = [3u64, 30, 300][round % 3];
            let records = (next() % 120) as usize;
            let mut buf = Vec::new();
            for _ in 0..records {
                let nfields = 1 + (next() % 5) as usize;
                for i in 0..nfields {
                    if i > 0 {
                        buf.push(b',');
                    }
                    if next() % dirt_rate == 0 {
                        buf.extend_from_slice(DIRT[(next() % DIRT.len() as u64) as usize]);
                    } else {
                        buf.extend_from_slice(WORDS[(next() % WORDS.len() as u64) as usize]);
                    }
                }
                buf.extend_from_slice(if next() % 4 == 0 { b"\r\n" } else { b"\n" });
            }
            if next() % 2 == 0 {
                buf.truncate((next() as usize) % (buf.len() + 1));
            }
            let mut want = Offsets::default();
            find_indexes_csv_lenient(&buf, &mut want);
            let mut got = Offsets::default();
            unsafe { generic::find_indexes_csv::<V>(&buf, &mut got, 0, 0) };
            assert_eq!(
                got.rel.fields,
                want.rel.fields,
                "buf={:?}",
                String::from_utf8_lossy(&buf)
            );
            let p = (next() as usize) % (buf.len() + 1);
            check_resumed_scan(&buf, p, |b, off| unsafe {
                generic::find_indexes_csv::<V>(b, off, 0, 0)
            });
        }
    }

    #[test]
    fn csv_irregular_quotes_match_lenient_generic() {
        csv_irregular_quotes_match_lenient::<generic::Impl>();
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn csv_irregular_quotes_match_lenient_neon() {
        csv_irregular_quotes_match_lenient::<neon::Impl>();
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn csv_irregular_quotes_match_lenient_sse2() {
        if is_x86_feature_detected!("sse2") && is_x86_feature_detected!("pclmulqdq") {
            csv_irregular_quotes_match_lenient::<sse2::Impl>();
        }
        if is_x86_feature_detected!("avx2") {
            csv_irregular_quotes_match_lenient::<avx2::Impl>();
        }
    }

    #[test]
    fn long_records_across_buffer_growth() {
        // Records much longer than the chunk size: the buffer is doubled several times before a
        // record separator shows up, and each doubling continues the previous scan.
        let corpus: String = [1usize, 3000, 7, 20000, 0, 513, 64, 65, 4100]
            .iter()
            .enumerate()
            .map(|(i, &nf)| {
                let fields: Vec<String> = (0..nf).map(|j| format!("f{i}w{j}")).collect();
                format!("{}\n", fields.join(if i % 2 == 0 { " " } else { "\t " }))
            })
            .collect();
        let corpus: &'static str = Box::leak(corpus.into_boxed_str());
        for used in [FieldSet::all(), FieldSet::singleton(0), FieldSet::empty()] {
            let want: Vec<(String, usize)> = corpus
                .lines()
                .map(|l| (l.to_string(), l.split_whitespace().count()))
                .collect();
            let br = ByteReader::new_whitespace(
                iter::once((io::Cursor::new(corpus), String::from("-"))),
                64,
                true,
                ExecutionStrategy::Serial,
                CancelSignal::default(),
            );
            assert_eq!(read_records(br, used.clone(), " "), want);
            let want: Vec<(String, usize)> = corpus
                .lines()
                // An empty record has no fields.
                .map(|l| {
                    (
                        l.to_string(),
                        if l.is_empty() {
                            0
                        } else {
                            l.split('w').count()
                        },
                    )
                })
                .collect();
            let br = ByteReader::new(
                iter::once((io::Cursor::new(corpus), String::from("-"))),
                b'w',
                b'\n',
                64,
                true,
                ExecutionStrategy::Serial,
                CancelSignal::default(),
            );
            assert_eq!(read_records(br, used, "w"), want);
        }
        // CSV, with quoted fields holding newlines and commas.
        let csv: String = (0..3)
            .map(|i| {
                let fields: Vec<String> = (0..2000 * (i + 1))
                    .map(|j| match j % 3 {
                        0 => format!("\"q,{j}\nx\""),
                        1 => format!("p{j}"),
                        _ => String::new(),
                    })
                    .collect();
                format!("{}\r\n", fields.join(","))
            })
            .collect();
        let want: Vec<Vec<String>> = csv
            .split("\r\n")
            .filter(|l| !l.is_empty())
            .map(|l| split_record(l.as_bytes(), InputFormat::CSV))
            .map(|fs| fs.iter().map(|s| s.to_string()).collect())
            .collect();
        assert_eq!(want[0][0], "q,0\nx");
        assert_eq!(csv_read_all(&csv, 64), want);
    }

    fn bytes_split(kernel: BytesIndexKernel, fs: u8, rs: u8, corpus: &'static str) {
        let mut _cache = RegexCache::default();
        let _pat = Str::default();
        let mut expected_lines: Vec<Str<'static>> = Vec::new();
        let mut expected: Vec<Vec<Str<'static>>> = corpus
            .split(rs as char)
            .map(|line| {
                expected_lines.push(Str::from(line));
                if line.is_empty() {
                    // For an empty line, Awk semantics are to have 0 fields.
                    Vec::new()
                } else {
                    line.split(fs as char)
                        .map(|x| Str::from(x).unmoor())
                        .collect()
                }
            })
            .collect();

        // For buffers that end in "\n" we don't want a trailing empty field.
        if corpus.as_bytes().last() == Some(&b'\n') {
            let _ = expected_lines.pop();
            let _ = expected.pop();
        }

        let reader = std::io::Cursor::new(corpus);
        let mut reader = ByteReader::new_internal(
            iter::once((reader, String::from("fake-stdin"))),
            fs,
            rs,
            1024,
            /*check_utf8=*/ true,
            ExecutionStrategy::Serial,
            kernel,
            Default::default(),
        );
        let mut got_lines = Vec::new();
        let mut got = Vec::new();
        loop {
            let (_, line) = reader
                .read_line(&_pat, &mut _cache)
                .expect("failed to read line");
            if reader.read_state() != 1 {
                break;
            }
            got_lines.push(line.line.clone());
            got.push(line.fields.clone());
        }
        if got != expected || got_lines != expected_lines {
            eprintln!(
                "test failed! got vector of length {} and {} , expected {} lines",
                got.len(),
                got_lines.len(),
                expected.len()
            );
            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                if g != e {
                    eprintln!("===============");
                    eprintln!("line {} has a mismatch", i);
                    eprintln!("got:  {}", disp_vec(g));
                    eprintln!("want: {}", disp_vec(e));
                }
            }
            for (i, (g, e)) in got_lines.iter().zip(expected_lines.iter()).enumerate() {
                if g != e {
                    eprintln!("===============");
                    eprintln!("line {} has a mismatch ($0)", i);
                    eprintln!("got:  {:?}", format!("{}", g));
                    eprintln!("want: {:?}", format!("{}", e));
                }
            }
            panic!("test failed. See debug output");
        }
    }

    fn bytes_splitter_generic<V: generic::Vector>() {
        let k = generic::find_indexes_byte::<V>;

        // Basic functionality
        bytes_split(k, b' ', b'\n', crate::test_string_constants::VIRGIL);
        bytes_split(
            k,
            b' ',
            b'\n',
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
        );

        // Lots of fields, but line separator not present
        bytes_split(
            k,
            b' ',
            0u8,
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
        );

        // Many lines, lots of single fields
        bytes_split(
            k,
            0u8,
            b'\n',
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
        );

        // One line, One field
        bytes_split(
            k,
            0u8,
            0u8,
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
        );

        // Trailing separators
        bytes_split(
            k,
            b' ',
            b'\n',
            "   leading whitespace   \n and some    more\n",
        );
    }

    #[test]
    fn bytes_splitter() {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if is_x86_feature_detected!("avx2") {
                    bytes_splitter_generic::<avx2::Impl>()
                }
                if is_x86_feature_detected!("sse2") {
                    bytes_splitter_generic::<sse2::Impl>()
                }
            } else if #[cfg(target_arch = "aarch64")] {
                bytes_splitter_generic::<neon::Impl>();
            }
        }
        bytes_splitter_generic::<generic::Impl>()
    }

    fn multithreaded_count<LR: LineReader + 'static>(
        corpus: &'static str,
        n_threads: usize,
        make_br: impl Fn(io::Cursor<&'static str>) -> LR,
    ) {
        use crate::common::Notification;
        use std::sync::Arc;
        use std::thread;
        let mut expected: usize = corpus.split('\n').count();
        let notify = Arc::new(Notification::default());

        // For buffers that end in "\n" we don't want a trailing empty field.
        if corpus.as_bytes().last() == Some(&b'\n') {
            expected = expected.checked_sub(1).unwrap();
        }
        let reader = make_br(std::io::Cursor::new(corpus));
        let others = reader.request_handles(n_threads);
        assert_eq!(others.len(), n_threads);
        let mut threads = Vec::new();
        for t in others.into_iter() {
            let n = notify.clone();
            threads.push(thread::spawn(move || {
                let mut _cache = RegexCache::default();
                let _pat = Str::default();
                let mut got = 0;
                n.wait();
                let mut reader = t();
                loop {
                    let (_, _line) = reader
                        .read_line(&_pat, &mut _cache)
                        .expect("failed to read line");
                    if reader.read_state() != 1 {
                        break;
                    }
                    got += 1;
                }
                got
            }));
        }
        notify.notify();
        let mut got_total = 0;
        for thr in threads.into_iter() {
            got_total += thr.join().expect("thread died unexpectedly");
        }
        assert_eq!(expected, got_total);
    }

    #[test]
    fn br_multithreaded_count() {
        fn make_br_ws(reader: impl io::Read + Send + 'static) -> impl LineReader {
            ByteReader::new_whitespace(
                iter::once((reader, String::from("fake-stdin"))),
                /*chunk_size=*/ 1024,
                /*check_utf8=*/ false,
                ExecutionStrategy::ShardPerRecord(4),
                Default::default(),
            )
        }
        fn make_br(reader: impl io::Read + Send + 'static) -> impl LineReader {
            ByteReader::new(
                iter::once((reader, String::from("fake-stdin"))),
                /*field_sep=*/ b' ',
                /*record_sep=*/ b'\n',
                /*chunk_size=*/ 1024,
                /*check_utf8=*/ false,
                ExecutionStrategy::ShardPerRecord(4),
                Default::default(),
            )
        }
        multithreaded_count(
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
            4,
            make_br_ws,
        );
        multithreaded_count(crate::test_string_constants::VIRGIL, 4, make_br_ws);
        multithreaded_count(
            "   leading whitespace   \n and some    more\n",
            2,
            make_br_ws,
        );
        multithreaded_count(
            crate::test_string_constants::PRIDE_PREJUDICE_CH2,
            4,
            make_br,
        );
        multithreaded_count(crate::test_string_constants::VIRGIL, 4, make_br);
        multithreaded_count("   leading whitespace   \n and some    more\n", 2, make_br);
    }

    fn whitespace_split(kernel: WhitespaceIndexKernel, corpus: &'static str) {
        let mut _cache = RegexCache::default();
        let _pat = Str::default();
        let mut expected_lines: Vec<Str<'static>> = Vec::new();
        let mut expected: Vec<Vec<Str<'static>>> = corpus
            .split('\n')
            .map(|line| {
                expected_lines.push(line.into());
                line.split(|c: char| c.is_ascii_whitespace())
                    // trim of leading and trailing whitespace.
                    .filter(|x| !x.is_empty())
                    .map(|x| Str::from(x).unmoor())
                    .collect()
            })
            .collect();

        // For buffers that end in "\n" we don't want a trailing empty field.
        if corpus.as_bytes().last() == Some(&b'\n') {
            let _ = expected_lines.pop();
            let _ = expected.pop();
        }
        let reader = std::io::Cursor::new(corpus);
        let mut reader = ByteReader::new_whitespace_internal(
            std::iter::once((reader, String::from("fake-stdin"))),
            1024,
            /*check_utf8=*/ false,
            ExecutionStrategy::Serial,
            kernel,
            Default::default(),
        );
        let mut got_lines = Vec::new();
        let mut got = Vec::new();
        loop {
            let (_, line) = reader
                .read_line(&_pat, &mut _cache)
                .expect("failed to read line");
            if reader.read_state() != 1 {
                break;
            }
            got_lines.push(line.line.clone());
            got.push(line.fields.clone());
        }
        if got != expected || got_lines != expected_lines {
            eprintln!(
                "test failed! got vector of length {} and {}, expected {} lines",
                got.len(),
                got_lines.len(),
                expected.len()
            );
            for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
                if g != e {
                    eprintln!("===============");
                    eprintln!("line {} has a mismatch (fields)", i);
                    eprintln!("got:  {}", disp_vec(g));
                    eprintln!("want: {}", disp_vec(e));
                }
            }
            for (i, (g, e)) in got_lines.iter().zip(expected_lines.iter()).enumerate() {
                if g != e {
                    eprintln!("===============");
                    eprintln!("line {} has a mismatch ($0)", i);
                    eprintln!("got:  {:?}", format!("{}", g));
                    eprintln!("want: {:?}", format!("{}", e));
                }
            }
            panic!("test failed. See debug output");
        }
    }

    fn whitespace_splitter_generic<V: generic::Vector>() {
        let k = generic::find_indexes_ascii_whitespace::<V>;
        whitespace_split(k, crate::test_string_constants::PRIDE_PREJUDICE_CH2);
        whitespace_split(k, crate::test_string_constants::VIRGIL);
        whitespace_split(k, "   leading whitespace   \n and some    more\n");
        whitespace_split(
            k,
            r#"xxxxxxxxxxxxxxxxxxxxxxxxxxxxx  yyyyyyyyyyyyyyyyyyyyyyyyyyyy 111111
xxxxxxxxxxxxxxxxxxxxxxxxxxx    yyyyyyyyyyyyyyyyyyyyyyyy     222222
xxxxxxxxxxxxxxxxxxxxxxxxxxxx  yyyyyyyyyyyyyyyyyyyyyyyyyyyy 3333333
xxxxxxxxxxxxxxxxxxxxxxxxxx    yyyyyyyyyyyyyyyyyyyyyyyy     4444444
"#,
        );
    }

    #[test]
    fn whitespace_splitter() {
        cfg_if::cfg_if! {
            if #[cfg(target_arch = "x86_64")] {
                if is_x86_feature_detected!("avx2") {
                    whitespace_splitter_generic::<avx2::Impl>()
                }
                if is_x86_feature_detected!("sse2") {
                    whitespace_splitter_generic::<sse2::Impl>()
                }
                if is_x86_feature_detected!("ssse3") {
                    whitespace_splitter_generic::<sse2::Impl<true>>()
                }
            } else if #[cfg(target_arch = "aarch64")] {
                whitespace_splitter_generic::<neon::Impl>();
            }
        }
        whitespace_splitter_generic::<generic::Impl>()
    }
}
