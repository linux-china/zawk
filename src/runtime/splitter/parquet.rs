//! Apache Parquet (`-i parquet`) input support.
//!
//! A Parquet file is converted to JSON Lines, one JSON object per row, which is then read like
//! `-i jsonl` input: `$1..$NF` are the top-level columns in schema order, `FI` maps each column
//! name to its index, and `$0` is the row as a JSON object. Going through the JSON Lines reader
//! (rather than a reader type of its own) keeps the binary small, as the interpreter and the
//! compiled runtime are generic over the reader type.
//!
//! Columns are decoded a batch of rows at a time with the column readers of the `parquet` crate
//! (without Arrow, also to keep the binary small): top-level primitive columns are written to
//! the JSON text straight from the decoded values, nested columns (LIST, MAP, STRUCT) go through
//! the record API. Row groups are decoded in parallel on background threads, and their rows are
//! passed on in file order. Value mapping, following `-i jsonl`: numbers (decimals exactly),
//! strings, dates, times and timestamps (UTC) are plain text, booleans are `1`/`0`, null is empty
//! (or the `--null` text), binary values are hex, and nested values (LIST, MAP, STRUCT) and JSON
//! columns are JSON text.
//!
//! Parquet keeps its metadata at the end of the file, so standard input and S3 objects
//! (`s3://bucket/key`) are read into memory.
//!
//! Only the columns that the program reads are decoded (see [`Projection`]): the others are
//! still keys of the JSON object, so that `NF` and `FI` do not change, but their values are null.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};

use ::parquet::basic::{ConvertedType, LogicalType, TimeUnit, Type as PhysicalType};
use ::parquet::bloom_filter::Sbbf;
use ::parquet::column::page::PageReader;
use ::parquet::column::reader::{ColumnReader, ColumnReaderImpl};
use ::parquet::data_type::{
    BoolType, ByteArray, ByteArrayType, Decimal, DoubleType, FixedLenByteArray,
    FixedLenByteArrayType, FloatType, Int32Type, Int64Type, Int96, Int96Type,
};
use ::parquet::errors::ParquetError;
use ::parquet::file::metadata::{
    ColumnChunkMetaData, FileMetaData, ParquetMetaData, RowGroupMetaData,
};
use ::parquet::file::reader::{
    ChunkReader, FileReader, Length, RowGroupReader, SerializedFileReader,
};
use ::parquet::record::Field;
use ::parquet::record::reader::RowIter;
use ::parquet::schema::types::{ColumnDescPtr, SchemaDescriptor, Type, TypePtr};
use bytes::Bytes;

use crate::inspect::Cell;
use crate::pushdown::FieldSet;

/// Whether `file` names a Parquet file: it ends with `.parquet` and, when it is a local file,
/// starts with the `PAR1` magic. Remote objects (S3, HTTP) are told by their extension only.
pub fn is_parquet_file(file: &str) -> bool {
    let name = file.split('?').next().unwrap_or_default();
    if !name.to_ascii_lowercase().ends_with(".parquet") {
        return false;
    }
    let path = std::path::Path::new(file);
    if !path.is_file() {
        return true;
    }
    let mut magic = [0u8; 4];
    File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map(|_| &magic == b"PAR1")
        .unwrap_or(true)
}

/// The columns of Parquet input that a program reads, from the used-field analysis of
/// `crate::pushdown`: columns read by position (`$2`) and columns looked up by name in `FI`.
/// Columns outside the projection are not decoded, which is what makes a columnar format fast.
pub struct Projection {
    fields: FieldSet,
    // The constant keys of the `FI` lookups, or None if `FI` is looked up with other keys.
    names: Option<Vec<String>>,
    // The constant keys that `-i parquet` adds to `FI` when the input has no such column.
    extra: Vec<String>,
    // The column names of the first (non-empty) file: positions are the columns of the first
    // record, as `-i jsonl` numbers them, later files only adding columns of new names.
    first: OnceLock<Vec<String>>,
}

impl Projection {
    pub fn new(fields: FieldSet, names: Option<Vec<String>>, extra: Vec<String>) -> Projection {
        Projection {
            fields,
            names,
            extra,
            first: OnceLock::new(),
        }
    }

    /// Which of `columns` (the column names of a file, in order) are read, or None for all.
    fn select(&self, columns: &[String]) -> Option<Vec<bool>> {
        // `$0` is the whole row, and a full set means the analysis could not tell.
        if self.fields == FieldSet::all() || self.fields.get(0) {
            return None;
        }
        let first = self.first.get_or_init(|| columns.to_vec());
        let mut wanted: HashSet<&str> = HashSet::new();
        if self.fields.has_fi() {
            for name in self.names.as_ref()? {
                // The used fields do not count a lookup of FI as a use of `$0`, which a lookup of
                // a key that FI does not hold would be.
                if !first.contains(name) && !self.extra.contains(name) {
                    return None;
                }
                wanted.insert(name.as_str());
            }
        }
        for i in 1..=62 {
            if self.fields.get(i) {
                // A column that the first file does not have: a later file names it.
                wanted.insert(first.get(i - 1)?.as_str());
            }
        }
        Some(
            columns
                .iter()
                .map(|c| wanted.contains(c.as_str()))
                .collect(),
        )
    }
}

/// How a value is written in JSON.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Kind {
    Null,
    Bool,
    /// The text is valid JSON as is: numbers and nested values.
    Raw,
    Str,
}

/// Special formatting of a top-level column, from its logical type.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Plain,
    TimestampNanos,
    TimeNanos,
    Uuid,
    Json,
}

enum Source {
    File(String),
    Stdin,
}

/// The rows of a Parquet file (or of standard input) as JSON Lines.
pub struct ParquetJson {
    source: Source,
    stream: Option<Stream>,
    buf: Vec<u8>,
    pos: usize,
    done: bool,
    projection: Option<Arc<Projection>>,
}

/// The rows of `-i parquet` for `zawk inspect`, read with the record API.
struct Rows {
    iter: RowIter<'static>,
    columns: Vec<TypePtr>,
    formats: Vec<Format>,
    // Columns that are not read (written as null): columns whose type the record API cannot read,
    // and columns outside the projection.
    skipped: Vec<bool>,
}

impl ParquetJson {
    pub fn file(path: String) -> ParquetJson {
        ParquetJson::new(Source::File(path))
    }
    pub fn stdin() -> ParquetJson {
        ParquetJson::new(Source::Stdin)
    }
    fn new(source: Source) -> ParquetJson {
        ParquetJson {
            source,
            stream: None,
            buf: Vec::new(),
            pos: 0,
            done: false,
            projection: None,
        }
    }

    /// Read only the columns of `projection`.
    pub fn with_projection(mut self, projection: Arc<Projection>) -> ParquetJson {
        self.projection = Some(projection);
        self
    }

    /// Check up front that `path` is a Parquet file that can be read, so that problems are
    /// reported clearly before any input is processed (rather than as a read error).
    pub fn check(path: &str) -> std::result::Result<(), String> {
        use ::parquet::basic::Compression;
        let error =
            |e: &dyn std::fmt::Display| format!("cannot read parquet file `{}': {}", path, e);
        // S3 objects are checked by the caller (`s3::check_object`), and remote objects are not
        // checked here: reading the metadata would download the whole object an extra time.
        if crate::runtime::s3::is_s3_url(path) || crate::runtime::http::is_http_url(path) {
            return Ok(());
        }
        let file = File::open(path).map_err(|e| {
            format!(
                "zawk: {}{}",
                crate::common::FATAL_PREFIX,
                crate::common::InputOpenError::new_io(path, &e)
            )
        })?;
        let reader = SerializedFileReader::new(file).map_err(|e| error(&e))?;
        for row_group in reader.metadata().row_groups() {
            for column in row_group.columns() {
                match column.compression() {
                    Compression::UNCOMPRESSED
                    | Compression::SNAPPY
                    | Compression::ZSTD(_)
                    | Compression::LZ4
                    | Compression::LZ4_RAW => {}
                    other => {
                        let name = match other {
                            Compression::GZIP(_) => "GZIP".to_string(),
                            Compression::BROTLI(_) => "BROTLI".to_string(),
                            other => other.to_string(),
                        };
                        return Err(error(&format!(
                            "unsupported compression {} (supported: SNAPPY, ZSTD, LZ4)",
                            name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        match &self.source {
            Source::File(path) => path,
            Source::Stdin => "-",
        }
    }

    fn error(&self, e: impl std::fmt::Display) -> io::Error {
        io::Error::other(format!("cannot read parquet file `{}': {}", self.name(), e))
    }

    /// The reader of the file, and its top-level column names (made unique).
    fn open_reader(&self) -> io::Result<(Box<dyn FileReader>, Vec<String>)> {
        let mut reader: Box<dyn FileReader> = match &self.source {
            Source::File(path) if crate::runtime::s3::is_s3_url(path) => {
                // Parquet needs random access: read the whole object into memory.
                let data =
                    crate::runtime::s3::read_object_bytes(path).map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(data).map_err(|e| self.error(e))?)
            }
            Source::File(path) if crate::runtime::http::is_http_url(path) => {
                let data = crate::runtime::http::read_bytes(path).map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(data).map_err(|e| self.error(e))?)
            }
            Source::File(path) => {
                let file = PosFile::open(path).map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(file).map_err(|e| self.error(e))?)
            }
            Source::Stdin => {
                let mut data = Vec::new();
                io::stdin()
                    .read_to_end(&mut data)
                    .map_err(|e| self.error(e))?;
                Box::new(SerializedFileReader::new(Bytes::from(data)).map_err(|e| self.error(e))?)
            }
        };
        // Repeated top-level names (`a`, `a`) are renamed as DuckDB does (`a`, `a_1`): as JSON keys
        // they would collide, and the record API would read the last such column for all of them.
        let fields = reader.metadata().file_metadata().schema().get_fields();
        let names = super::unique_names(fields.iter().map(|f| f.name()));
        if fields.iter().zip(&names).any(|(f, name)| f.name() != name) {
            reader = Box::new(RenamedReader::new(reader, &names).map_err(|e| self.error(e))?);
        }
        Ok((reader, names))
    }

    /// The top-level columns of the file, their formats, and which of them cannot be read (with
    /// a warning for each).
    fn layout(&self, reader: &dyn FileReader) -> (Vec<TypePtr>, Vec<Format>, Vec<bool>) {
        let root = reader.metadata().file_metadata().schema_descr_ptr();
        let columns: Vec<TypePtr> = root.root_schema().get_fields().to_vec();
        let formats = columns.iter().map(|c| column_format(c)).collect();
        let unsupported: Vec<bool> = columns.iter().map(|c| !supported(c)).collect();
        for (column, bad) in columns.iter().zip(&unsupported) {
            if *bad {
                crate::runtime::stdlib_warning(
                    "parquet",
                    format!(
                        "column `{}' of `{}' has an unsupported type, it is read as empty",
                        column.name(),
                        self.name()
                    ),
                );
            }
        }
        (columns, formats, unsupported)
    }

    /// The rows of the file, for `zawk inspect` (all columns, with the record API).
    fn open(&self) -> io::Result<Rows> {
        let (reader, _) = self.open_reader()?;
        let root = reader.metadata().file_metadata().schema_descr_ptr();
        let (columns, formats, unsupported) = self.layout(&*reader);
        // Skip the unsupported columns (the record API panics on them).
        let skipped = unsupported;
        let projection = if skipped.iter().any(|skip| *skip) {
            let fields: Vec<TypePtr> = columns
                .iter()
                .zip(&skipped)
                .filter(|(_, skip)| !**skip)
                .map(|(c, _)| c.clone())
                .collect();
            let projection = Type::group_type_builder(root.root_schema().name())
                .with_fields(fields)
                .build()
                .map_err(|e| self.error(e))?;
            Some(projection)
        } else {
            None
        };
        let iter = RowIter::from_file_into(reader)
            .project(projection)
            .map_err(|e| self.error(e))?;
        Ok(Rows {
            iter,
            columns,
            formats,
            skipped,
        })
    }

    /// How each top-level column is decoded, from the schema and the projection.
    fn decoder(&self) -> io::Result<Decoder> {
        let (reader, names) = self.open_reader()?;
        let (columns, formats, unsupported) = self.layout(&*reader);
        let selected = match &self.projection {
            // An empty file has no first record: positions refer to the next file.
            Some(p) if reader.metadata().file_metadata().num_rows() > 0 => p.select(&names),
            _ => None,
        };
        let descr = reader.metadata().file_metadata().schema_descr_ptr();
        // The leaf column of each top-level primitive column.
        let mut leaf_of = vec![None; columns.len()];
        for leaf in 0..descr.num_columns() {
            leaf_of[descr.get_column_root_idx(leaf)] = Some(leaf);
        }
        let mut record_fields = Vec::new();
        let mut plans = Vec::with_capacity(columns.len());
        for (i, column) in columns.iter().enumerate() {
            let read = selected.as_ref().is_none_or(|s| s[i]);
            let plan = if unsupported[i] || !read {
                // Unsupported columns (the record API panics on them) and the columns that the
                // program does not read are null.
                Plan::Skip
            } else if column.is_primitive()
                && let Some(leaf) = leaf_of[i]
                && descr.column(leaf).max_rep_level() == 0
            {
                Plan::Leaf(leaf)
            } else {
                record_fields.push(column.clone());
                Plan::Record
            };
            plans.push(plan);
        }
        let record = if record_fields.is_empty() {
            None
        } else {
            let projection = Type::group_type_builder(descr.root_schema().name())
                .with_fields(record_fields)
                .build()
                .map_err(|e| self.error(e))?;
            Some(projection)
        };
        let keys = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut key = String::from(if i == 0 { "{" } else { "," });
                push_json_str(&mut key, c.name());
                key.push(':');
                key
            })
            .collect();
        Ok(Decoder {
            reader,
            formats,
            plans,
            keys,
            record,
        })
    }

    /// Start decoding the row groups on background threads.
    fn start(&self) -> io::Result<Stream> {
        let decoder = Arc::new(self.decoder()?);
        let row_groups = decoder.reader.num_row_groups();
        let mut senders = Vec::with_capacity(row_groups);
        let mut receivers = Vec::with_capacity(row_groups);
        for _ in 0..row_groups {
            // A worker runs at most this many batches ahead of the reader in its row group.
            let (tx, rx) = sync_channel(2);
            senders.push(Some(tx));
            receivers.push(rx);
        }
        let senders = Arc::new(Mutex::new(senders));
        let next = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(false));
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(row_groups);
        for _ in 0..threads {
            let (decoder, senders, next, cancel) = (
                decoder.clone(),
                senders.clone(),
                next.clone(),
                cancel.clone(),
            );
            let name = self.name().to_string();
            let worker = move || {
                // Row groups are taken in order, so the one the reader waits for is always being
                // decoded (or done): the bounded channels cannot deadlock.
                loop {
                    let rg = next.fetch_add(1, Ordering::Relaxed);
                    if rg >= row_groups || cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    let tx: SyncSender<io::Result<Vec<u8>>> =
                        senders.lock().unwrap()[rg].take().unwrap();
                    let mut emit =
                        |buf: Vec<u8>| !cancel.load(Ordering::Relaxed) && tx.send(Ok(buf)).is_ok();
                    if let Err(e) = decoder.decode(rg, &mut emit) {
                        let msg = format!("cannot read parquet file `{}': {}", name, e);
                        let _ = tx.send(Err(io::Error::other(msg)));
                    }
                }
            };
            std::thread::Builder::new()
                .name("zawk-parquet".to_string())
                .spawn(worker)?;
        }
        Ok(Stream {
            receivers,
            current: 0,
            cancel,
        })
    }

    /// Fill the buffer with the next batch of rows as JSON Lines.
    fn fill(&mut self) -> io::Result<()> {
        if self.stream.is_none() {
            self.stream = Some(self.start()?);
        }
        let stream = self.stream.as_mut().unwrap();
        self.pos = 0;
        while let Some(rx) = stream.receivers.get(stream.current) {
            match rx.recv() {
                Ok(Ok(buf)) => {
                    self.buf = buf;
                    return Ok(());
                }
                Ok(Err(e)) => return Err(e),
                // The row group is done.
                Err(_) => stream.current += 1,
            }
        }
        self.buf.clear();
        self.done = true;
        Ok(())
    }
}

/// The batches of JSON Lines of the row groups, decoded on background threads; a receiver for
/// each row group, read in order.
struct Stream {
    receivers: Vec<Receiver<io::Result<Vec<u8>>>>,
    current: usize,
    cancel: Arc<AtomicBool>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        // The rest of the input is not read (`exit`): stop the workers.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// How a top-level column is decoded.
enum Plan {
    /// Not read: written as null.
    Skip,
    /// A primitive column, decoded with the column reader of this leaf column.
    Leaf(usize),
    /// A nested column, decoded with the record API.
    Record,
}

/// The rows of row groups as JSON Lines.
struct Decoder {
    reader: Box<dyn FileReader>,
    formats: Vec<Format>,
    plans: Vec<Plan>,
    // The JSON text before the value of each column: `{"name":` or `,"name":`.
    keys: Vec<String>,
    // The nested columns that are read, for the record API.
    record: Option<Type>,
}

/// The rows decoded at a time.
const BATCH_ROWS: usize = 4096;

impl Decoder {
    /// Decode row group `rg`, passing each batch of rows to `emit`, until `emit` returns false.
    fn decode(
        &self,
        rg: usize,
        emit: &mut dyn FnMut(Vec<u8>) -> bool,
    ) -> ::parquet::errors::Result<()> {
        let group = self.reader.get_row_group(rg)?;
        let mut left = usize::try_from(group.metadata().num_rows()).unwrap_or(0);
        let mut leaves = Vec::with_capacity(self.plans.len());
        for plan in &self.plans {
            leaves.push(match plan {
                Plan::Leaf(i) => {
                    let descr = group.metadata().schema_descr().column(*i);
                    Some(Leaf::new(group.get_column_reader(*i)?, descr))
                }
                _ => None,
            });
        }
        let mut records = match &self.record {
            Some(projection) => Some(RowIter::from_row_group(Some(projection.clone()), &*group)?),
            None => None,
        };
        let mut capacity = 0;
        while left > 0 {
            let n = left.min(BATCH_ROWS);
            for leaf in leaves.iter_mut().flatten() {
                leaf.fill(n)?;
            }
            let mut out = String::with_capacity(capacity);
            for _ in 0..n {
                let row = match records.as_mut() {
                    Some(rows) => match rows.next() {
                        Some(row) => Some(row?),
                        None => return Err(ParquetError::General("missing rows".to_string())),
                    },
                    None => None,
                };
                let mut fields = row.as_ref().map(|row| row.get_column_iter());
                for (i, plan) in self.plans.iter().enumerate() {
                    out.push_str(&self.keys[i]);
                    match plan {
                        Plan::Skip => out.push_str("null"),
                        Plan::Leaf(_) => {
                            let leaf = leaves[i].as_mut().unwrap();
                            leaf.push_next(&mut out, self.formats[i])?;
                        }
                        Plan::Record => match fields.as_mut().and_then(|f| f.next()) {
                            Some((_, field)) => {
                                let (text, kind) = field_text(field, self.formats[i]);
                                push_json(&mut out, &text, kind);
                            }
                            None => out.push_str("null"),
                        },
                    }
                }
                if self.plans.is_empty() {
                    out.push('{');
                }
                out.push_str("}\n");
            }
            left -= n;
            capacity = capacity.max(out.len());
            if !emit(out.into_bytes()) {
                break;
            }
        }
        Ok(())
    }
}

/// The decoded values of a batch of rows of a primitive column.
enum Values {
    Bool(ColumnReaderImpl<BoolType>, Vec<bool>),
    Int32(ColumnReaderImpl<Int32Type>, Vec<i32>),
    Int64(ColumnReaderImpl<Int64Type>, Vec<i64>),
    Int96(ColumnReaderImpl<Int96Type>, Vec<Int96>),
    Float(ColumnReaderImpl<FloatType>, Vec<f32>),
    Double(ColumnReaderImpl<DoubleType>, Vec<f64>),
    Bytes(ColumnReaderImpl<ByteArrayType>, Vec<ByteArray>),
    Fixed(
        ColumnReaderImpl<FixedLenByteArrayType>,
        Vec<FixedLenByteArray>,
    ),
}

/// A primitive top-level column of a row group, read a batch of rows at a time.
struct Leaf {
    descr: ColumnDescPtr,
    converted: ConvertedType,
    max_def: i16,
    // The definition levels of the batch (when the column is optional), and the next one.
    defs: Vec<i16>,
    def_pos: usize,
    // The non-null values of the batch, and the next one.
    values: Values,
    pos: usize,
}

impl Leaf {
    fn new(reader: ColumnReader, descr: ColumnDescPtr) -> Leaf {
        let values = match reader {
            ColumnReader::BoolColumnReader(r) => Values::Bool(r, Vec::new()),
            ColumnReader::Int32ColumnReader(r) => Values::Int32(r, Vec::new()),
            ColumnReader::Int64ColumnReader(r) => Values::Int64(r, Vec::new()),
            ColumnReader::Int96ColumnReader(r) => Values::Int96(r, Vec::new()),
            ColumnReader::FloatColumnReader(r) => Values::Float(r, Vec::new()),
            ColumnReader::DoubleColumnReader(r) => Values::Double(r, Vec::new()),
            ColumnReader::ByteArrayColumnReader(r) => Values::Bytes(r, Vec::new()),
            ColumnReader::FixedLenByteArrayColumnReader(r) => Values::Fixed(r, Vec::new()),
        };
        Leaf {
            converted: descr.converted_type(),
            max_def: descr.max_def_level(),
            descr,
            defs: Vec::new(),
            def_pos: 0,
            values,
            pos: 0,
        }
    }

    /// Decode the next `n` rows.
    fn fill(&mut self, n: usize) -> ::parquet::errors::Result<()> {
        self.defs.clear();
        self.def_pos = 0;
        self.pos = 0;
        let defs = if self.max_def > 0 {
            Some(&mut self.defs)
        } else {
            None
        };
        let read = match &mut self.values {
            Values::Bool(r, v) => read_rows(r, n, defs, v),
            Values::Int32(r, v) => read_rows(r, n, defs, v),
            Values::Int64(r, v) => read_rows(r, n, defs, v),
            Values::Int96(r, v) => read_rows(r, n, defs, v),
            Values::Float(r, v) => read_rows(r, n, defs, v),
            Values::Double(r, v) => read_rows(r, n, defs, v),
            Values::Bytes(r, v) => read_rows(r, n, defs, v),
            Values::Fixed(r, v) => read_rows(r, n, defs, v),
        }?;
        if read < n {
            return Err(ParquetError::General(format!(
                "column `{}' has {} values fewer than the rows of its row group",
                self.descr.path(),
                n - read
            )));
        }
        Ok(())
    }

    /// Write the value of the next row as JSON.
    fn push_next(&mut self, out: &mut String, format: Format) -> ::parquet::errors::Result<()> {
        use ConvertedType as C;
        if self.max_def > 0 {
            let def = self.defs[self.def_pos];
            self.def_pos += 1;
            if def < self.max_def {
                out.push_str("null");
                return Ok(());
            }
        }
        let i = self.pos;
        self.pos += 1;
        // The common types are written directly; the others (and the columns with a special
        // format) as the record API reads them, so that their text is the same.
        let plain = format == Format::Plain;
        let field = match &self.values {
            Values::Bool(_, v) => {
                out.push_str(if v[i] { "true" } else { "false" });
                return Ok(());
            }
            Values::Int32(_, v) => {
                let v = v[i];
                let int = match self.converted {
                    C::NONE | C::INT_32 => Some(v as i64),
                    C::INT_16 => Some(v as i16 as i64),
                    C::INT_8 => Some(v as i8 as i64),
                    C::UINT_32 => Some(v as u32 as i64),
                    C::UINT_16 => Some(v as u16 as i64),
                    C::UINT_8 => Some(v as u8 as i64),
                    _ => None,
                };
                match int {
                    Some(int) if plain => {
                        out.push_str(itoa::Buffer::new().format(int));
                        return Ok(());
                    }
                    _ => Field::convert_int32(&self.descr, v),
                }
            }
            Values::Int64(_, v) => match self.converted {
                C::NONE | C::INT_64 if plain => {
                    out.push_str(itoa::Buffer::new().format(v[i]));
                    return Ok(());
                }
                C::UINT_64 if plain => {
                    out.push_str(itoa::Buffer::new().format(v[i] as u64));
                    return Ok(());
                }
                _ => Field::convert_int64(&self.descr, v[i]),
            },
            Values::Int96(_, v) => Field::convert_int96(&self.descr, v[i]),
            Values::Float(_, v) => {
                push_float(out, v[i] as f64, Some(v[i]));
                return Ok(());
            }
            Values::Double(_, v) => {
                push_float(out, v[i], None);
                return Ok(());
            }
            Values::Bytes(_, v) => match self.converted {
                C::UTF8 | C::ENUM if plain => match std::str::from_utf8(v[i].data()) {
                    Ok(s) => {
                        push_json_str(out, s);
                        return Ok(());
                    }
                    // the error of the record API
                    Err(_) => Field::convert_byte_array(&self.descr, v[i].clone())?,
                },
                C::NONE | C::BSON if plain => {
                    out.push('"');
                    push_hex(out, v[i].data());
                    out.push('"');
                    return Ok(());
                }
                _ => Field::convert_byte_array(&self.descr, v[i].clone())?,
            },
            Values::Fixed(_, v) => {
                Field::convert_byte_array(&self.descr, ByteArray::from(v[i].clone()))?
            }
        };
        let (text, kind) = field_text(&field, format);
        push_json(out, &text, kind);
        Ok(())
    }
}

/// Read the next `n` rows (records) of a column, appending to `defs` and `values`; the number of
/// rows read is less than `n` only at the end of the column chunk.
fn read_rows<T: ::parquet::data_type::DataType>(
    reader: &mut ColumnReaderImpl<T>,
    n: usize,
    mut defs: Option<&mut Vec<i16>>,
    values: &mut Vec<T::T>,
) -> ::parquet::errors::Result<usize> {
    values.clear();
    let mut read = 0;
    while read < n {
        let (records, _, _) = reader.read_records(n - read, defs.as_deref_mut(), None, values)?;
        if records == 0 {
            break;
        }
        read += records;
    }
    Ok(read)
}

/// A local file read with positional reads, so that the row groups can be decoded on several
/// threads at once (the readers of `File` share its offset).
struct PosFile {
    file: Arc<File>,
    len: u64,
}

impl PosFile {
    fn open(path: &str) -> io::Result<PosFile> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(PosFile {
            file: Arc::new(file),
            len,
        })
    }
}

impl Length for PosFile {
    fn len(&self) -> u64 {
        self.len
    }
}

impl ChunkReader for PosFile {
    type T = io::BufReader<PosRead>;

    fn get_read(&self, start: u64) -> ::parquet::errors::Result<Self::T> {
        Ok(io::BufReader::new(PosRead {
            file: self.file.clone(),
            pos: start,
        }))
    }

    fn get_bytes(&self, start: u64, length: usize) -> ::parquet::errors::Result<Bytes> {
        let mut buf = vec![0u8; length];
        let mut done = 0;
        while done < length {
            match read_at(&self.file, &mut buf[done..], start + done as u64) {
                Ok(0) => {
                    return Err(ParquetError::EOF(format!(
                        "Expected to read {} bytes, read only {}",
                        length, done
                    )));
                }
                Ok(n) => done += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(buf.into())
    }
}

/// A reader of a file from an offset, with positional reads.
struct PosRead {
    file: Arc<File>,
    pos: u64,
}

impl Read for PosRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = read_at(&self.file, buf, self.pos)?;
        self.pos += n as u64;
        Ok(n)
    }
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

/// A file reader whose top-level columns are renamed. The record API maps each leaf column to its
/// column chunk by path, so the schema and the column chunk metadata both use the new names; the
/// pages are still read by the wrapped reader.
struct RenamedReader {
    inner: Box<dyn FileReader>,
    metadata: ParquetMetaData,
}

impl RenamedReader {
    fn new(
        inner: Box<dyn FileReader>,
        names: &[String],
    ) -> ::parquet::errors::Result<RenamedReader> {
        let file = inner.metadata().file_metadata();
        let schema = file.schema();
        let mut fields = Vec::with_capacity(names.len());
        for (field, name) in schema.get_fields().iter().zip(names) {
            fields.push(Arc::new(renamed(field, name)?));
        }
        let schema = Type::group_type_builder(schema.name())
            .with_fields(fields)
            .build()?;
        let descr = Arc::new(SchemaDescriptor::new(Arc::new(schema)));
        let mut row_groups = Vec::with_capacity(inner.num_row_groups());
        for row_group in inner.metadata().row_groups() {
            let mut columns = Vec::with_capacity(row_group.num_columns());
            for (i, c) in row_group.columns().iter().enumerate() {
                let column = ColumnChunkMetaData::builder(descr.column(i))
                    .set_encodings(c.encodings().collect())
                    .set_num_values(c.num_values())
                    .set_compression(c.compression())
                    .set_total_compressed_size(c.compressed_size())
                    .set_total_uncompressed_size(c.uncompressed_size())
                    .set_data_page_offset(c.data_page_offset())
                    .set_dictionary_page_offset(c.dictionary_page_offset())
                    .set_index_page_offset(c.index_page_offset())
                    .build()?;
                columns.push(column);
            }
            let row_group = RowGroupMetaData::builder(descr.clone())
                .set_num_rows(row_group.num_rows())
                .set_total_byte_size(row_group.total_byte_size())
                .set_column_metadata(columns)
                .build()?;
            row_groups.push(row_group);
        }
        let file = FileMetaData::new(
            file.version(),
            file.num_rows(),
            file.created_by().map(str::to_string),
            file.key_value_metadata().cloned(),
            descr,
            file.column_orders().cloned(),
        );
        Ok(RenamedReader {
            inner,
            metadata: ParquetMetaData::new(file, row_groups),
        })
    }
}

/// `t` with the name `name`.
fn renamed(t: &Type, name: &str) -> ::parquet::errors::Result<Type> {
    let info = t.get_basic_info();
    let id = if info.has_id() { Some(info.id()) } else { None };
    match t {
        Type::PrimitiveType {
            physical_type,
            type_length,
            scale,
            precision,
            ..
        } => Type::primitive_type_builder(name, *physical_type)
            .with_repetition(info.repetition())
            .with_converted_type(info.converted_type())
            .with_logical_type(info.logical_type_ref().cloned())
            .with_length(*type_length)
            .with_precision(*precision)
            .with_scale(*scale)
            .with_id(id)
            .build(),
        Type::GroupType { fields, .. } => Type::group_type_builder(name)
            .with_repetition(info.repetition())
            .with_converted_type(info.converted_type())
            .with_logical_type(info.logical_type_ref().cloned())
            .with_fields(fields.clone())
            .with_id(id)
            .build(),
    }
}

impl FileReader for RenamedReader {
    fn metadata(&self) -> &ParquetMetaData {
        &self.metadata
    }
    fn num_row_groups(&self) -> usize {
        self.inner.num_row_groups()
    }
    fn get_row_group(&self, i: usize) -> ::parquet::errors::Result<Box<dyn RowGroupReader + '_>> {
        Ok(Box::new(RenamedRowGroup {
            inner: self.inner.get_row_group(i)?,
            metadata: &self.metadata.row_groups()[i],
        }))
    }
    fn get_row_iter(&self, projection: Option<Type>) -> ::parquet::errors::Result<RowIter<'_>> {
        RowIter::from_file(projection, self)
    }
}

struct RenamedRowGroup<'a> {
    inner: Box<dyn RowGroupReader + 'a>,
    metadata: &'a RowGroupMetaData,
}

impl RowGroupReader for RenamedRowGroup<'_> {
    fn metadata(&self) -> &RowGroupMetaData {
        self.metadata
    }
    fn num_columns(&self) -> usize {
        self.inner.num_columns()
    }
    fn get_column_page_reader(&self, i: usize) -> ::parquet::errors::Result<Box<dyn PageReader>> {
        self.inner.get_column_page_reader(i)
    }
    fn get_column_bloom_filter(&self, i: usize) -> Option<&Sbbf> {
        self.inner.get_column_bloom_filter(i)
    }
    fn get_row_iter(&self, projection: Option<Type>) -> ::parquet::errors::Result<RowIter<'_>> {
        RowIter::from_row_group(projection, self)
    }
}

/// The columns and rows of a Parquet file as cells, for `zawk inspect`.
pub struct ParquetTable {
    rows: Rows,
    name: String,
}

impl ParquetJson {
    pub fn table(self) -> io::Result<ParquetTable> {
        let rows = self.open()?;
        Ok(ParquetTable {
            rows,
            name: self.name().to_string(),
        })
    }
}

impl ParquetTable {
    /// The top-level column names and their types (`INT64`, `STRING`, `DECIMAL(10,2)`, ...).
    pub fn columns(&self) -> Vec<(String, String)> {
        self.rows
            .columns
            .iter()
            .map(|c| (c.name().to_string(), type_name(c)))
            .collect()
    }

    pub fn next_row(&mut self) -> Option<io::Result<Vec<Cell>>> {
        let rows = &mut self.rows;
        let row = match rows.iter.next()? {
            Ok(row) => row,
            Err(e) => {
                let msg = format!("cannot read parquet file `{}': {}", self.name, e);
                return Some(Err(io::Error::other(msg)));
            }
        };
        let mut values = row.get_column_iter();
        let mut cells = Vec::with_capacity(rows.columns.len());
        for i in 0..rows.columns.len() {
            if rows.skipped[i] {
                cells.push(Cell::Null);
                continue;
            }
            cells.push(match values.next() {
                Some((_, field)) => field_cell(field, rows.formats[i]),
                None => Cell::Null,
            });
        }
        Some(Ok(cells))
    }
}

fn field_cell(field: &Field, format: Format) -> Cell {
    match field {
        Field::Null => return Cell::Null,
        Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
            return Cell::Nested(field_text(field, format).0);
        }
        _ => {}
    }
    let (text, kind) = field_text(field, format);
    match kind {
        _ if format == Format::Json => Cell::Nested(text),
        Kind::Bool => Cell::Bool(text == "1"),
        // NaN and infinities: null in JSON, but numbers to awk
        Kind::Null | Kind::Raw => Cell::Num(text),
        Kind::Str => Cell::Str(text),
    }
}

/// The type of a top-level column, named as in `-o parquet` headers where possible.
fn type_name(t: &Type) -> String {
    use ConvertedType as C;
    let info = t.get_basic_info();
    let converted = info.converted_type();
    if t.is_group() {
        let name = match (converted, info.logical_type_ref()) {
            (C::LIST, _) | (_, Some(LogicalType::List)) => "LIST",
            (C::MAP | C::MAP_KEY_VALUE, _) | (_, Some(LogicalType::Map)) => "MAP",
            _ => "STRUCT",
        };
        return name.to_string();
    }
    let name = match info.logical_type_ref() {
        Some(LogicalType::Uuid) => "UUID",
        Some(LogicalType::Float16) => "FLOAT16",
        Some(LogicalType::Timestamp(_)) => "TIMESTAMP",
        Some(LogicalType::Time(_)) => "TIME",
        Some(LogicalType::Json) => "JSON",
        Some(LogicalType::String | LogicalType::Enum) => "STRING",
        _ => match converted {
            C::UTF8 | C::ENUM => "STRING",
            C::JSON => "JSON",
            C::BSON => "BSON",
            C::DECIMAL => return format!("DECIMAL({},{})", t.get_precision(), t.get_scale()),
            C::DATE => "DATE",
            C::TIME_MILLIS | C::TIME_MICROS => "TIME",
            C::TIMESTAMP_MILLIS | C::TIMESTAMP_MICROS => "TIMESTAMP",
            C::INT_8 => "INT8",
            C::INT_16 => "INT16",
            C::INT_32 => "INT32",
            C::INT_64 => "INT64",
            C::UINT_8 => "UINT8",
            C::UINT_16 => "UINT16",
            C::UINT_32 => "UINT32",
            C::UINT_64 => "UINT64",
            C::INTERVAL => "INTERVAL",
            _ => match t.get_physical_type() {
                PhysicalType::BOOLEAN => "BOOLEAN",
                PhysicalType::INT32 => "INT32",
                PhysicalType::INT64 => "INT64",
                PhysicalType::INT96 => "INT96",
                PhysicalType::FLOAT => "FLOAT",
                PhysicalType::DOUBLE => "DOUBLE",
                PhysicalType::BYTE_ARRAY | PhysicalType::FIXED_LEN_BYTE_ARRAY => "BINARY",
            },
        },
    };
    name.to_string()
}

impl Read for ParquetJson {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            if self.done {
                return Ok(0);
            }
            self.fill()?;
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

pub(super) fn push_json(out: &mut String, text: &str, kind: Kind) {
    match kind {
        Kind::Null => out.push_str("null"),
        Kind::Bool => out.push_str(if text == "1" { "true" } else { "false" }),
        Kind::Raw => out.push_str(text),
        Kind::Str => push_json_str(out, text),
    }
}

/// A JSON string literal.
pub(super) fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    // Copy the runs of bytes that need no escape at once; the escaped bytes are all ASCII, so
    // the runs are whole characters.
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let escape = match b {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0..=0x1f => "",
            _ => continue,
        };
        out.push_str(&s[start..i]);
        start = i + 1;
        if escape.is_empty() {
            out.push_str(&format!("\\u{:04x}", b));
        } else {
            out.push_str(escape);
        }
    }
    out.push_str(&s[start..]);
    out.push('"');
}

/// A float as JSON: the text of [`float_text`], null for infinities and NaN.
fn push_float(out: &mut String, v: f64, single: Option<f32>) {
    if !v.is_finite() {
        out.push_str("null");
        return;
    }
    let mut buf = ryu::Buffer::new();
    let text = match single {
        Some(f) => buf.format_finite(f),
        None => buf.format_finite(v),
    };
    out.push_str(text.strip_suffix(".0").unwrap_or(text));
}

/// The bytes in lowercase hex, as [`hex`].
fn push_hex(out: &mut String, bytes: &[u8]) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.reserve(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
}

/// Whether the record API can read every value of this column (it panics on some types, such
/// as INTERVAL).
fn supported(t: &Type) -> bool {
    if t.is_group() {
        return t.get_fields().iter().all(|f| supported(f));
    }
    use ConvertedType as C;
    let converted = t.get_basic_info().converted_type();
    match t.get_physical_type() {
        PhysicalType::BOOLEAN
        | PhysicalType::INT96
        | PhysicalType::FLOAT
        | PhysicalType::DOUBLE => true,
        PhysicalType::INT32 => matches!(
            converted,
            C::NONE
                | C::INT_8
                | C::INT_16
                | C::INT_32
                | C::UINT_8
                | C::UINT_16
                | C::UINT_32
                | C::DATE
                | C::TIME_MILLIS
                | C::DECIMAL
        ),
        PhysicalType::INT64 => matches!(
            converted,
            C::NONE
                | C::INT_64
                | C::UINT_64
                | C::TIME_MICROS
                | C::TIMESTAMP_MILLIS
                | C::TIMESTAMP_MICROS
                | C::DECIMAL
        ),
        PhysicalType::BYTE_ARRAY => matches!(
            converted,
            C::NONE | C::UTF8 | C::ENUM | C::JSON | C::BSON | C::DECIMAL
        ),
        PhysicalType::FIXED_LEN_BYTE_ARRAY => matches!(converted, C::NONE | C::DECIMAL),
    }
}

fn column_format(t: &Type) -> Format {
    if t.is_group() {
        return Format::Plain;
    }
    if t.get_basic_info().converted_type() == ConvertedType::JSON {
        return Format::Json;
    }
    match t.get_basic_info().logical_type_ref() {
        Some(LogicalType::Timestamp(ts)) if ts.unit == TimeUnit::NANOS => Format::TimestampNanos,
        Some(LogicalType::Time(time)) if time.unit == TimeUnit::NANOS => Format::TimeNanos,
        Some(LogicalType::Uuid) => Format::Uuid,
        Some(LogicalType::Json) => Format::Json,
        _ => Format::Plain,
    }
}

/// The awk text of a top-level value, and how it is written in the JSON of `$0`.
fn field_text(field: &Field, format: Format) -> (String, Kind) {
    match (field, format) {
        (Field::Long(nanos), Format::TimestampNanos) => (timestamp_nanos(*nanos), Kind::Str),
        (Field::Long(nanos), Format::TimeNanos) => (time_nanos(*nanos), Kind::Str),
        (Field::Bytes(b), Format::Uuid) if b.data().len() == 16 => (uuid(b.data()), Kind::Str),
        // JSON columns keep their text; it is embedded as is in $0 when it is valid JSON.
        (Field::Str(s), Format::Json) => {
            let valid = serde_json::from_str::<serde::de::IgnoredAny>(s).is_ok();
            (s.clone(), if valid { Kind::Raw } else { Kind::Str })
        }
        (Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_), _) => {
            let mut json = String::new();
            write_json(&mut json, field);
            (json, Kind::Raw)
        }
        _ => scalar_text(field),
    }
}

/// The awk text of a scalar value, and how it is written in JSON.
fn scalar_text(field: &Field) -> (String, Kind) {
    let int = |v: i128| (v.to_string(), Kind::Raw);
    match field {
        Field::Null => (String::new(), Kind::Null),
        Field::Bool(b) => ((if *b { "1" } else { "0" }).to_string(), Kind::Bool),
        Field::Byte(v) => int(*v as i128),
        Field::Short(v) => int(*v as i128),
        Field::Int(v) => int(*v as i128),
        Field::Long(v) => int(*v as i128),
        Field::UByte(v) => int(*v as i128),
        Field::UShort(v) => int(*v as i128),
        Field::UInt(v) => int(*v as i128),
        Field::ULong(v) => int(*v as i128),
        Field::Float16(v) => float_text(v.to_f32() as f64, Some(v.to_f32())),
        Field::Float(v) => float_text(*v as f64, Some(*v)),
        Field::Double(v) => float_text(*v, None),
        Field::Decimal(d) => match decimal_text(d) {
            Some(text) => (text, Kind::Raw),
            None => (hex(d.data()), Kind::Str),
        },
        Field::Str(s) => (s.clone(), Kind::Str),
        Field::Bytes(b) => (hex(b.data()), Kind::Str),
        Field::Date(days) => (date(*days), Kind::Str),
        Field::TimeMillis(ms) => (time_nanos(*ms as i64 * 1_000_000), Kind::Str),
        Field::TimeMicros(us) => (time_nanos(us * 1_000), Kind::Str),
        Field::TimestampMillis(ms) => (timestamp_nanos(ms.saturating_mul(1_000_000)), Kind::Str),
        Field::TimestampMicros(us) => (timestamp_nanos(us.saturating_mul(1_000)), Kind::Str),
        Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
            let mut json = String::new();
            write_json(&mut json, field);
            (json, Kind::Raw)
        }
    }
}

/// A float in its shortest form, without a trailing ".0" (`3.5`, `100`, `1e300`). Infinities
/// and NaN are not JSON numbers: they are written as null in JSON.
pub(super) fn float_text(v: f64, single: Option<f32>) -> (String, Kind) {
    if !v.is_finite() {
        let text = if v.is_nan() {
            "nan"
        } else if v > 0.0 {
            "inf"
        } else {
            "-inf"
        };
        return (text.to_string(), Kind::Null);
    }
    let mut buf = ryu::Buffer::new();
    // the shortest text of a FLOAT is that of the f32 (1.1, not 1.100000023841858)
    let text = match single {
        Some(f) => buf.format_finite(f),
        None => buf.format_finite(v),
    };
    (
        text.strip_suffix(".0").unwrap_or(text).to_string(),
        Kind::Raw,
    )
}

/// Write a value as JSON (nested values: LIST as an array, MAP and STRUCT as an object).
fn write_json(out: &mut String, field: &Field) {
    match field {
        Field::Group(row) => {
            out.push('{');
            for (i, (name, value)) in row.get_column_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                push_json_str(out, name);
                out.push(':');
                write_json(out, value);
            }
            out.push('}');
        }
        Field::ListInternal(list) => {
            out.push('[');
            for (i, value) in list.elements().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json(out, value);
            }
            out.push(']');
        }
        Field::MapInternal(map) => {
            out.push('{');
            for (i, (key, value)) in map.entries().iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                // object keys are strings
                let key = match key {
                    Field::Group(_) | Field::ListInternal(_) | Field::MapInternal(_) => {
                        let mut json = String::new();
                        write_json(&mut json, key);
                        json
                    }
                    _ => scalar_text(key).0,
                };
                push_json_str(out, &key);
                out.push(':');
                write_json(out, value);
            }
            out.push('}');
        }
        _ => {
            let (text, kind) = scalar_text(field);
            push_json(out, &text, kind);
        }
    }
}

/// Exact text of a decimal, e.g. `12345.67` or `-0.50` (None if it does not fit in 128 bits).
fn decimal_text(d: &Decimal) -> Option<String> {
    let bytes = d.data();
    if bytes.is_empty() || bytes.len() > 16 {
        return None;
    }
    // big-endian two's complement
    let mut value: i128 = if bytes[0] & 0x80 != 0 { -1 } else { 0 };
    for b in bytes {
        value = (value << 8) | *b as i128;
    }
    let digits = value.unsigned_abs().to_string();
    let sign = if value < 0 { "-" } else { "" };
    let scale = d.scale().max(0) as usize;
    if scale == 0 {
        return Some(format!("{sign}{digits}"));
    }
    let digits = format!("{:0>width$}", digits, width = scale + 1);
    let (int, frac) = digits.split_at(digits.len() - scale);
    Some(format!("{sign}{int}.{frac}"))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn uuid(b: &[u8]) -> String {
    let h = hex(b);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub(super) fn date(days: i32) -> String {
    chrono::DateTime::from_timestamp(days as i64 * 86_400, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `2024-01-15 10:30:00`, with the fraction of a second only when it is not zero.
pub(super) fn timestamp_nanos(nanos: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(nanos)
        .naive_utc()
        .to_string()
}

pub(super) fn time_nanos(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let hms = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if frac == 0 {
        hms
    } else {
        let frac = format!("{:09}", frac);
        format!("{}.{}", hms, frac.trim_end_matches('0'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::parquet::data_type::ByteArray;

    #[test]
    fn decimals_are_exact() {
        assert_eq!(
            decimal_text(&Decimal::from_i32(1234567, 9, 2)).unwrap(),
            "12345.67"
        );
        assert_eq!(
            decimal_text(&Decimal::from_i64(-50, 18, 2)).unwrap(),
            "-0.50"
        );
        assert_eq!(decimal_text(&Decimal::from_i32(5, 9, 0)).unwrap(), "5");
        assert_eq!(
            decimal_text(&Decimal::from_i32(-7, 9, 3)).unwrap(),
            "-0.007"
        );
        let big = Decimal::from_bytes(ByteArray::from(i128::MAX.to_be_bytes().to_vec()), 38, 10);
        assert_eq!(
            decimal_text(&big).unwrap(),
            "17014118346046923173168730371.5884105727"
        );
    }

    #[test]
    fn scalars() {
        assert_eq!(float_text(3.5, None), ("3.5".to_string(), Kind::Raw));
        assert_eq!(float_text(100.0, None), ("100".to_string(), Kind::Raw));
        assert_eq!(
            float_text(1.1f32 as f64, Some(1.1f32)),
            ("1.1".to_string(), Kind::Raw)
        );
        assert_eq!(float_text(f64::NAN, None).1, Kind::Null);
        assert_eq!(time_nanos(49_530_500_000_000), "13:45:30.5");
        assert_eq!(time_nanos(3_600_000_000_000), "01:00:00");
        assert_eq!(
            timestamp_nanos(1_705_314_600_123_456_789),
            "2024-01-15 10:30:00.123456789"
        );
        assert_eq!(
            scalar_text(&Field::Bool(true)),
            ("1".to_string(), Kind::Bool)
        );
        assert_eq!(scalar_text(&Field::Null), (String::new(), Kind::Null));
    }

    /// Rows of several row groups (decoded on several threads) come out in file order, with the
    /// nulls of optional columns in place.
    #[test]
    fn row_groups_in_order() {
        use ::parquet::data_type::{ByteArrayType, Int32Type, Int64Type};
        use ::parquet::file::properties::WriterProperties;
        use ::parquet::file::writer::SerializedFileWriter;
        use ::parquet::schema::parser::parse_message_type;

        let schema = parse_message_type(
            "message m { required int64 id; optional binary name (UTF8); optional int32 x; }",
        )
        .unwrap();
        let path = std::env::temp_dir().join(format!(
            "zawk-parquet-row-groups-{}.parquet",
            std::process::id()
        ));
        let mut expected = String::new();
        {
            let file = File::create(&path).unwrap();
            let props = Arc::new(WriterProperties::builder().build());
            let mut writer = SerializedFileWriter::new(file, Arc::new(schema), props).unwrap();
            let mut id = 0i64;
            for rows in [5000usize, 3, 0, 9000] {
                let mut group = writer.next_row_group().unwrap();
                let ids: Vec<i64> = (id..id + rows as i64).collect();
                let name_defs: Vec<i16> = ids.iter().map(|i| (i % 3 != 0) as i16).collect();
                let names: Vec<ByteArray> = ids
                    .iter()
                    .filter(|i| *i % 3 != 0)
                    .map(|i| ByteArray::from(format!("n\"{i}").as_str()))
                    .collect();
                let x_defs: Vec<i16> = ids.iter().map(|i| (i % 2 == 0) as i16).collect();
                let xs: Vec<i32> = ids
                    .iter()
                    .filter(|i| *i % 2 == 0)
                    .map(|i| -(*i as i32))
                    .collect();
                let mut col = group.next_column().unwrap().unwrap();
                col.typed::<Int64Type>()
                    .write_batch(&ids, None, None)
                    .unwrap();
                col.close().unwrap();
                let mut col = group.next_column().unwrap().unwrap();
                col.typed::<ByteArrayType>()
                    .write_batch(&names, Some(&name_defs), None)
                    .unwrap();
                col.close().unwrap();
                let mut col = group.next_column().unwrap().unwrap();
                col.typed::<Int32Type>()
                    .write_batch(&xs, Some(&x_defs), None)
                    .unwrap();
                col.close().unwrap();
                group.close().unwrap();
                for i in ids {
                    let name = if i % 3 != 0 {
                        format!("\"n\\\"{i}\"")
                    } else {
                        "null".to_string()
                    };
                    let x = if i % 2 == 0 {
                        (-i).to_string()
                    } else {
                        "null".to_string()
                    };
                    expected.push_str(&format!("{{\"id\":{i},\"name\":{name},\"x\":{x}}}\n"));
                }
                id += rows as i64;
            }
            writer.close().unwrap();
        }
        let mut got = String::new();
        let result = ParquetJson::file(path.to_str().unwrap().to_string()).read_to_string(&mut got);
        std::fs::remove_file(&path).unwrap();
        result.unwrap();
        assert_eq!(got.len(), expected.len());
        assert!(got == expected);
    }

    #[test]
    fn json_strings() {
        let mut out = String::new();
        push_json_str(&mut out, "a\"b\\c\nd\te\u{1}张");
        assert_eq!(out, r#""a\"b\\c\nd\te\u0001张""#);
        assert!(serde_json::from_str::<String>(&out).is_ok());
    }
}
