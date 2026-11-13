//! Reading regular files in parallel (`-pr`): instead of one thread reading every chunk and the
//! workers scanning them, the reading thread only hands out byte ranges of the files, and each
//! worker reads its range itself with `pread` (see [BlockRead::read]). A single reading thread
//! tops out at about the speed of one `memcpy` from the page cache (~12 GB/s); this way the
//! copies run on every worker.
//!
//! A range is a block of `chunk_size` bytes. The worker of a block reads the records that *start*
//! in it: it skips the bytes up to the first record separator at or after the byte before the
//! block (the end of the previous block's last record), and reads on past the block up to the
//! first record separator at or after its last byte. So every record is read by exactly one
//! worker, and every chunk ends at a record separator (or at the end of the file), as the chunks
//! of the reading thread do with a deferred scan (see `ChunkProducer::defer_scan`).
//!
//! This needs the record boundaries to be found from any position: records separated by a single
//! byte (lines, `-F` with one byte, the default FS, TSV), not CSV, where a newline may be inside
//! quotes opened far before. It also needs the bytes of the file as they are: not compressed, and
//! not checked as UTF-8 (`--utf8`), which the reading thread does in order.
use std::fs::File;
use std::io;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::common::Result;
use crate::runtime::{
    decompress,
    splitter::{
        UTF8_BOM,
        chunk::{
            ChunkProducer, DeferredScan, LinesOnly, OffsetChunk, ScanOffsets, check_chunk_len,
        },
    },
    str_impl::UniqueBuf,
};

/// The zeroed bytes after the data of a chunk buffer, as in the buffers of `Reader`.
const PADDING: usize = 128;

/// The bytes read before the start of a block at a time, looking for the end of the previous
/// block's last record.
const HEAD_WINDOW: usize = 4 << 10;

/// An input file the workers can read in ranges.
pub struct PreadFile {
    file: Arc<File>,
    len: u64,
    name: Arc<str>,
}

impl PreadFile {
    /// The file at `path` (named `name` in `FILENAME`), when it is a regular file that is not
    /// compressed; `None` otherwise (including when it can't be opened: the usual reader then
    /// reports the error).
    pub fn open(path: &Path, name: &str) -> Option<PreadFile> {
        let file = File::open(path).ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() {
            return None;
        }
        let len = meta.len();
        let mut prefix = [0u8; 16];
        let n = read_full_at(&file, &mut prefix, 0).ok()?;
        if decompress::detect(&prefix[..n]).is_some() {
            return None;
        }
        Some(PreadFile {
            file: Arc::new(file),
            len,
            name: name.into(),
        })
    }
}

/// The input files of `-pr`, when all of them are regular files the workers can read in ranges
/// (see [PreadFile::open]). `files` holds the path of each operand and its name.
pub fn pread_inputs<'a>(
    files: impl IntoIterator<Item = (&'a Path, &'a str)>,
) -> Option<Vec<PreadFile>> {
    files
        .into_iter()
        .map(|(path, name)| PreadFile::open(path, name))
        .collect()
}

/// A block of a file left to the worker that takes the chunk (see the module comment).
pub struct BlockRead {
    file: Arc<File>,
    file_len: u64,
    /// The block, `start..end`: the records starting in it are read.
    start: u64,
    end: u64,
    /// The size of a chunk buffer, which buffers of the same size are reused for.
    buf_size: usize,
    record_sep: u8,
    scan: DeferredScan,
    lines_only: bool,
}

/// The bytes of a block, as read by [BlockRead::read].
pub struct BlockData {
    pub buf: UniqueBuf,
    pub len: usize,
    /// Whether the data runs to the end of the file (so it may not end with a record separator).
    pub at_eof: bool,
}

impl BlockRead {
    /// The scan to fill the offsets of the chunk read, and whether it only finds records.
    pub fn scan(&self, at_eof: bool) -> (DeferredScan, bool) {
        if self.lines_only {
            let scan = DeferredScan::Lines {
                record_sep: self.record_sep,
            };
            return (scan, true);
        }
        let mut scan = self.scan;
        // As in `OffsetChunkProducer::get_deferred_chunk`: the producer keeps every offset of the
        // last chunk of a file.
        if let (DeferredScan::Whitespace { trim, .. }, true) = (&mut scan, at_eof) {
            *trim = false;
        }
        (scan, false)
    }

    /// Reads the records that start in the block, the first one at `start` (see
    /// [BlockRead::first_record]), into `spare` if it has the size of a chunk buffer.
    pub fn read(&self, start: u64, spare: Option<UniqueBuf>) -> Result<BlockData> {
        let file_len = self.file_len;
        // The data ends at the first record separator at or after the last byte of the block,
        // which the read below goes past, or at the end of the file. (The first block may start
        // after its end, past a byte order mark.)
        let mut search_from = (self.end - 1).saturating_sub(start) as usize;
        let mut buf = match spare {
            Some(b) if b.len() == self.buf_size => b,
            _ => UniqueBuf::new(self.buf_size),
        };
        let mut filled = 0;
        let (len, at_eof) = loop {
            let room = buf.len() - PADDING;
            let want = room.min((file_len - start) as usize);
            let n = read_full_at(
                &self.file,
                &mut buf.as_mut_bytes()[filled..want],
                start + filled as u64,
            )
            .map_err(|e| err_raw!("failed to read input: {}", e))?;
            filled += n;
            let bs = &buf.as_bytes()[..filled];
            // `search_from` passes `filled` only when the file got shorter; the slice is then
            // empty and nothing is found, but use the clamped start for both to keep `len <= filled`.
            let base = search_from.min(filled);
            if let Some(i) = memchr::memchr(self.record_sep, &bs[base..]) {
                let len = base + i + 1;
                break (len, start + len as u64 == file_len);
            }
            // The end of the file (or of what is left of it, if it got shorter).
            if filled < want || start + filled as u64 == file_len {
                break (filled, true);
            }
            // The last record of the block goes on past the buffer: read on into a larger one.
            search_from = filled;
            check_chunk_len(room.saturating_mul(2))?;
            let mut larger = UniqueBuf::new(room * 2 + PADDING);
            larger.as_mut_bytes()[..filled].copy_from_slice(&buf.as_bytes()[..filled]);
            buf = larger;
        };
        check_chunk_len(len)?;
        // Zero the rest: the bytes read past the chunk, the padding, and what a reused buffer
        // held, so that every byte is written (see `UniqueBuf::set_ascii_checkable`).
        buf.as_mut_bytes()[len..].fill(0);
        buf.set_ascii_checkable();
        Ok(BlockData { buf, len, at_eof })
    }

    /// The offset of the first record starting in the block; `None` when there is none (the
    /// block is inside a longer record).
    pub fn first_record(&self) -> Result<Option<u64>> {
        if self.start == 0 {
            // As `Reader` does, drop the byte order mark at the start of the file.
            let mut head = [0u8; 3];
            let n = read_full_at(&self.file, &mut head, 0)
                .map_err(|e| err_raw!("failed to read input: {}", e))?;
            let skip = if head[..n] == *UTF8_BOM { 3 } else { 0 };
            // The first record is read with the first block, even when the block is all byte
            // order mark: the next blocks only see records after a record separator.
            return Ok(Some(skip).filter(|&s| s < self.file_len));
        }
        // A record starts after each record separator: look for the first one at or after the
        // byte before the block, before the last byte of the block.
        let mut window = [0u8; HEAD_WINDOW];
        let mut pos = self.start - 1;
        while pos < self.end - 1 {
            let want = HEAD_WINDOW.min((self.end - 1 - pos) as usize);
            let n = read_full_at(&self.file, &mut window[..want], pos)
                .map_err(|e| err_raw!("failed to read input: {}", e))?;
            if let Some(i) = memchr::memchr(self.record_sep, &window[..n]) {
                return Ok(Some(pos + i as u64 + 1));
            }
            if n < want {
                // The file got shorter.
                return Ok(None);
            }
            pos += n as u64;
        }
        Ok(None)
    }
}

/// Reads into all of `buf` from `offset`, unless the file ends first; returns the bytes read.
fn read_full_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match read_at(file, &mut buf[done..], offset + done as u64) {
            Ok(0) => break,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, offset)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    // `seek_read` moves the offset of the file, but every read here gives its own offset.
    std::os::windows::fs::FileExt::seek_read(file, buf, offset)
}

/// The producer run by the reading thread of `ParallelChunkProducer`: yields a chunk per block of
/// each file, with the block to read (see `Chunk::finish`), and keeps the buffer the chunk came
/// back with, for the worker to read into.
pub struct PreadChunkProducer<Off> {
    files: std::iter::Enumerate<std::vec::IntoIter<PreadFile>>,
    cur: Option<(PreadFile, /*version*/ u32)>,
    /// The start of the next block of `cur`.
    next: u64,
    block_size: u64,
    buf_size: usize,
    record_sep: u8,
    scan: DeferredScan,
    lines_only: Option<LinesOnly>,
    _off: PhantomData<fn() -> Off>,
}

impl<Off> PreadChunkProducer<Off> {
    /// `scan` is the scan of a chunk when it is not `lines_only`; `record_sep` ends the records.
    pub fn new(
        files: Vec<PreadFile>,
        chunk_size: usize,
        record_sep: u8,
        scan: DeferredScan,
        lines_only: Option<LinesOnly>,
    ) -> Self {
        let block_size = chunk_size.max(1);
        // Room for the last record of the block, which usually ends within a few bytes after it.
        let tail = (block_size / 32).clamp(256, 64 << 10);
        PreadChunkProducer {
            files: files.into_iter().enumerate(),
            cur: None,
            next: 0,
            block_size: block_size as u64,
            buf_size: block_size + tail + PADDING,
            record_sep,
            scan,
            lines_only,
            _off: PhantomData,
        }
    }
}

impl<Off: ScanOffsets> ChunkProducer for PreadChunkProducer<Off> {
    type Chunk = OffsetChunk<Off>;
    fn next_file(&mut self) -> Result<bool> {
        self.cur = None;
        Ok(self.files.len() > 0)
    }
    fn get_chunk(&mut self, chunk: &mut OffsetChunk<Off>) -> Result<bool> {
        loop {
            let (file, version) = match &self.cur {
                Some((f, v)) if self.next < f.len => (f, *v),
                _ => match self.files.next() {
                    Some((i, f)) => {
                        // Versions start at 1, as in `new_chained_offset_chunk_producer_bytes`.
                        self.cur = Some((f, (i as u32).wrapping_add(1)));
                        self.next = 0;
                        continue;
                    }
                    None => {
                        self.cur = None;
                        return Ok(true);
                    }
                },
            };
            let start = self.next;
            let end = file.len.min(start.saturating_add(self.block_size));
            self.next = end;
            chunk.version = version;
            chunk.name = file.name.clone();
            chunk.len = 0;
            chunk.off.clear_offsets();
            chunk.deferred = None;
            chunk.block = Some(BlockRead {
                file: file.file.clone(),
                file_len: file.len,
                start,
                end,
                buf_size: self.buf_size,
                record_sep: self.record_sep,
                scan: self.scan,
                lines_only: self
                    .lines_only
                    .as_ref()
                    .is_some_and(|l| l.load(Ordering::Relaxed)),
            });
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::splitter::batch::{
        InputFormat, Offsets, WhitespaceOffsets, get_find_indexes,
        get_find_indexes_ascii_whitespace, get_find_indexes_bytes,
    };
    use crate::runtime::splitter::chunk::{
        Chunk, new_chained_offset_chunk_producer_ascii_whitespace,
        new_chained_offset_chunk_producer_bytes, new_chained_offset_chunk_producer_csv,
    };
    use std::io::Write;
    use std::sync::atomic::AtomicBool;

    /// The bytes of each chunk with its offsets (`rel`, then `nl`) made absolute, and its version
    /// and name.
    type Stream = (Vec<u8>, Vec<u32>, Vec<u32>, Vec<(u32, String)>);

    /// The whitespace scan may put a field start at the end of a chunk ending in a blank (when
    /// the end is not a multiple of the vector size: it compares the zeroed padding after it),
    /// which no record reads. It depends on where the chunks end, so with `drop_end` the offsets
    /// at the end of each chunk are dropped; and so are they after a record separator.
    fn collect<Off: ScanOffsets, P: ChunkProducer<Chunk = OffsetChunk<Off>>>(
        mut p: P,
        offsets: impl Fn(&Off) -> &Offsets,
        drop_end: bool,
        record_sep: u8,
    ) -> Stream {
        let (mut data, mut rel, mut nl, mut files) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut chunk = OffsetChunk::<Off>::default();
        let mut prev = OffsetChunk::<Off>::default();
        while !p.get_chunk(&mut chunk).unwrap() {
            if !chunk.finish(&mut prev).unwrap() {
                continue;
            }
            let base = data.len() as u32;
            let off = offsets(&chunk.off);
            let bs = &chunk.buf.as_ref().unwrap().as_bytes()[..chunk.len];
            let ended = drop_end || bs.last() == Some(&record_sep);
            let keep = |&&o: &&u32| !ended || (o as usize) < bs.len();
            rel.extend(off.rel.fields.iter().filter(keep).map(|&o| base + o));
            nl.extend(off.nl.fields.iter().filter(keep).map(|&o| base + o));
            data.extend_from_slice(bs);
            let file = (chunk.version, chunk.name.to_string());
            if files.last() != Some(&file) {
                files.push(file);
            }
            // Hand the buffer back, as the readers do.
            prev.buf = chunk.buf.take();
        }
        (data, rel, nl, files)
    }

    /// Compares two streams, reporting the first difference compactly.
    fn check(got: &Stream, want: &Stream, ctx: &str) {
        if got == want {
            return;
        }
        let around = |v: &[u32], i: usize| v[i.saturating_sub(3)..(i + 3).min(v.len())].to_vec();
        let diff = |a: &[u32], b: &[u32]| {
            let i = a
                .iter()
                .zip(b)
                .position(|(x, y)| x != y)
                .unwrap_or(a.len().min(b.len()));
            let at = *a.get(i).or(b.get(i)).unwrap_or(&0) as usize;
            let bytes = String::from_utf8_lossy(
                &got.0[at.saturating_sub(12).min(got.0.len())..(at + 12).min(got.0.len())],
            )
            .into_owned();
            (i, around(a, i), around(b, i), bytes)
        };
        panic!(
            "{ctx}: data equal: {}, files {:?} vs {:?}\nrel {:?}\nnl {:?}",
            got.0 == want.0,
            got.3,
            want.3,
            diff(&got.1, &want.1),
            diff(&got.2, &want.2)
        );
    }

    fn temp_files(inputs: &[Vec<u8>]) -> (Vec<tempfile::NamedTempFile>, Vec<PreadFile>) {
        let temps: Vec<_> = inputs
            .iter()
            .map(|bs| {
                let mut f = tempfile::NamedTempFile::new().unwrap();
                f.write_all(bs).unwrap();
                f.flush().unwrap();
                f
            })
            .collect();
        let files = temps
            .iter()
            .enumerate()
            .map(|(i, t)| PreadFile::open(t.path(), &format!("f{i}")).unwrap())
            .collect();
        (temps, files)
    }

    fn named(inputs: &[Vec<u8>]) -> impl Iterator<Item = (&[u8], String)> {
        inputs
            .iter()
            .enumerate()
            .map(|(i, bs)| (bs.as_slice(), format!("f{i}")))
    }

    fn random_inputs() -> Vec<Vec<u8>> {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut inputs: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"\n".to_vec(),
            b"\n\n\n\n".to_vec(),
            b"a".to_vec(),
            b"a b,c\n".to_vec(),
            b"a b,c\nd".to_vec(),
            b"\xEF\xBB\xBFx,y\nz\n".to_vec(),
            b"\xEF\xBB\xBF".to_vec(),
            b"\xEF\xBB".to_vec(),
            // Records longer than the blocks.
            format!("short\n{}\nshort\n", "long ,\t".repeat(3000)).into_bytes(),
            "x,y\n".repeat(5000).into_bytes(),
        ];
        const ALPHABET: &[u8] = b"aaaaabc,,\t\t  \n\n\r;\xC3\xA9";
        for i in 0..60 {
            let len = next() % if i % 4 == 0 { 40000 } else { 3000 };
            let line = 1 + next() % if i % 3 == 0 { 2000 } else { 60 };
            let mut bs: Vec<u8> = (0..len)
                .map(|_| ALPHABET[(next() % ALPHABET.len() as u64) as usize])
                .collect();
            // Lines of up to `line` bytes, plus the newlines of the alphabet.
            for (j, b) in bs.iter_mut().enumerate() {
                if j as u64 % line == line - 1 {
                    *b = b'\n';
                }
            }
            if i % 5 == 0 {
                bs.splice(0..0, UTF8_BOM.iter().copied());
            }
            inputs.push(bs);
        }
        inputs
    }

    // `Reader` only drops a byte order mark that is all in its first read, so not with chunks of 1 or
    // 2 bytes.
    const CHUNK_SIZES: &[usize] = &[3, 7, 64, 1000, 4096, 1 << 20];

    // The workers' reads of blocks give the same bytes and offsets as the chunks of one reader,
    // for each kind of scan they are used for, several files at a time.
    #[test]
    fn pread_matches_reader_bytes() {
        let inputs = random_inputs();
        let kernel = get_find_indexes_bytes();
        for group in inputs.chunks(7) {
            let (_temps, _) = temp_files(group);
            for &chunk_size in CHUNK_SIZES {
                for (field_sep, record_sep) in [(b',', b'\n'), (b'\t', b'\n'), (b',', b';')] {
                    for lines_only in [false, true] {
                        let lo = || LinesOnly::new(AtomicBool::new(lines_only));
                        let want = collect(
                            new_chained_offset_chunk_producer_bytes(
                                named(group),
                                chunk_size,
                                field_sep,
                                record_sep,
                                false,
                                kernel,
                                lo(),
                            ),
                            |o: &Offsets| o,
                            false,
                            record_sep,
                        );
                        assert_eq!(
                            want.0,
                            group
                                .iter()
                                .flat_map(|b| { b.strip_prefix(UTF8_BOM).unwrap_or(b) })
                                .copied()
                                .collect::<Vec<_>>()
                        );
                        let (_temps, files) = temp_files(group);
                        let scan = DeferredScan::Bytes {
                            kernel,
                            field_sep,
                            record_sep,
                        };
                        let got = collect(
                            PreadChunkProducer::new(
                                files,
                                chunk_size,
                                record_sep,
                                scan,
                                Some(lo()),
                            ),
                            |o: &Offsets| o,
                            false,
                            record_sep,
                        );
                        assert_eq!(
                            got, want,
                            "chunk_size={chunk_size} sep={field_sep}/{record_sep} \
                             lines_only={lines_only}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn pread_matches_reader_whitespace() {
        let inputs = random_inputs();
        let kernel = get_find_indexes_ascii_whitespace();
        for group in inputs.chunks(7) {
            for &chunk_size in CHUNK_SIZES {
                for lines_only in [false, true] {
                    let lo = || LinesOnly::new(AtomicBool::new(lines_only));
                    let want = collect(
                        new_chained_offset_chunk_producer_ascii_whitespace(
                            named(group),
                            chunk_size,
                            false,
                            kernel,
                            lo(),
                        ),
                        |o: &WhitespaceOffsets| &o.0,
                        /*drop_end*/ true,
                        b'\n',
                    );
                    let (_temps, files) = temp_files(group);
                    let scan = DeferredScan::Whitespace { kernel, trim: true };
                    let got = collect(
                        PreadChunkProducer::new(files, chunk_size, b'\n', scan, Some(lo())),
                        |o: &WhitespaceOffsets| &o.0,
                        /*drop_end*/ true,
                        b'\n',
                    );
                    check(
                        &got,
                        &want,
                        &format!("chunk_size={chunk_size} lines_only={lines_only}"),
                    );
                }
            }
        }
    }

    #[test]
    fn pread_matches_reader_tsv() {
        let inputs = random_inputs();
        for group in inputs.chunks(7) {
            for &chunk_size in CHUNK_SIZES {
                let want = collect(
                    new_chained_offset_chunk_producer_csv(
                        named(group),
                        chunk_size,
                        InputFormat::TSV,
                        false,
                    ),
                    |o: &Offsets| o,
                    false,
                    b'\n',
                );
                let (_temps, files) = temp_files(group);
                let scan = DeferredScan::Csv {
                    kernel: get_find_indexes(InputFormat::TSV),
                    quoted: false,
                };
                let got = collect(
                    PreadChunkProducer::new(files, chunk_size, b'\n', scan, None),
                    |o: &Offsets| o,
                    false,
                    b'\n',
                );
                assert_eq!(got, want, "chunk_size={chunk_size}");
            }
        }
    }

    // Compressed files, and anything that is not a regular file, are left to the reader.
    #[test]
    fn pread_only_plain_regular_files() {
        let temps: Vec<_> = [&b"\x1f\x8b\x08\x00rest"[..], b"plain\n"]
            .iter()
            .map(|bs| {
                let mut f = tempfile::NamedTempFile::new().unwrap();
                f.write_all(bs).unwrap();
                f
            })
            .collect();
        assert!(PreadFile::open(temps[0].path(), "gz").is_none());
        assert!(PreadFile::open(temps[1].path(), "plain").is_some());
        let dir = tempfile::tempdir().unwrap();
        assert!(PreadFile::open(dir.path(), "dir").is_none());
        assert!(PreadFile::open(&dir.path().join("missing"), "missing").is_none());
        assert!(
            pread_inputs([(temps[1].path(), "a"), (temps[0].path(), "b")]).is_none(),
            "one file the workers can't read in ranges"
        );
    }
}
