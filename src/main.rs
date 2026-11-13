#![recursion_limit = "1024"]
#![allow(unsafe_op_in_unsafe_fn)]

#[macro_use]
pub mod common;

pub mod arena;
pub mod ast;
pub mod builtins;
pub mod bytecode;
pub mod cfg;
#[macro_use]
pub mod codegen;
mod auto_parallel;
pub mod awk_util;
pub mod compile;
pub mod cross_stage;
pub mod dataflow;
mod display;
pub mod dom;
mod fmt;
mod function_docs;
#[cfg(test)]
pub mod harness;
mod include;
mod inspect;
pub mod interp;
pub mod lexer;
mod lint;
mod llm;
#[allow(unused_parens)] // Warnings appear in generated code
#[allow(clippy::all)]
pub mod parsing;
pub mod pushdown;
pub mod runtime;
mod string_constants;
mod strnum_analysis;
mod test_runner;
#[cfg(test)]
mod test_string_constants;
pub mod types;
mod uninit;

use clap::{Arg, Command};

use arena::Arena;
use cfg::Escaper;
use codegen::intrinsics::IntoRuntime;
use common::{CancelSignal, ExecutionStrategy, Stage};
use runtime::{
    CHUNK_SIZE, ChainedReader, LineReader, PARALLEL_CHUNK_SIZE,
    splitter::{
        avro::{self, AvroJson},
        batch::{ByteReader, CSVReader, InputFormat},
        database,
        json::JsonRecords,
        jsonl::JsonlReader,
        kafka,
        markdown::MarkdownCsv,
        message::{self, MessageReader, MessageSource},
        nats,
        parquet::{self, ParquetJson},
        regex::RegexSplitter,
        xlsx,
    },
};
use std::fs::{File, Permissions};
use std::io::{self, BufReader, Write};
use std::iter::once;
use std::mem;
use std::sync::Arc;

#[cfg(target_family = "unix")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Exit codes follow gawk: 1 for usage and syntax errors (`fail!`), including the static errors
// gawk finds while parsing (see `common::SYNTAX_PREFIX`), 2 for other fatal errors
// (`fatal!`): errors found while compiling (e.g. an undefined function) or running the program,
// and files that cannot be read or written.
// Messages are reported in the gawk style: `zawk: msg` and `zawk: fatal: msg`.
macro_rules! fail {
    ($($t:tt)*) => {{
        let msg = format!($($t)*);
        if msg.starts_with("zawk: ") {
            eprintln_ignore!("{}", msg);
        } else {
            eprintln_ignore!("zawk: {}", msg);
        }
        std::process::exit(1)
    }}
}

macro_rules! fatal {
    ($($t:tt)*) => {{
        eprintln_ignore!("{}", common::fatal_message(&format!($($t)*)));
        runtime::stats::report(2);
        std::process::exit(2)
    }}
}

/// The arguments not valid UTF-8, by the text `command_line_args` gives them: the original bytes of
/// a file name (see `raw_path`).
static RAW_ARGS: std::sync::OnceLock<std::collections::HashMap<String, std::ffi::OsString>> =
    std::sync::OnceLock::new();

/// The command-line arguments. clap and zawk take them as `String`s, so the bytes of an argument
/// that are not valid UTF-8 (a Latin-1 or GBK file name, `-F $'\xa7'`) are written as `\xNN`
/// escapes: `-F`, `-v`, `var=value` operands and string literals of the program decode them back to
/// the bytes, and a file name is opened by its original bytes.
fn command_line_args() -> Vec<String> {
    let mut raw = std::collections::HashMap::new();
    let args = std::env::args_os()
        .map(|arg| match arg.into_string() {
            Ok(arg) => arg,
            Err(arg) => {
                let text = escape_non_utf8(&arg);
                raw.insert(text.clone(), arg);
                text
            }
        })
        .collect();
    let _ = RAW_ARGS.set(raw);
    args
}

#[cfg(unix)]
fn escape_non_utf8(arg: &std::ffi::OsStr) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut text = String::new();
    for chunk in arg.as_bytes().utf8_chunks() {
        text.push_str(chunk.valid());
        for b in chunk.invalid() {
            let _ = write!(text, "\\x{:02X}", b);
        }
    }
    text
}

#[cfg(not(unix))]
fn escape_non_utf8(arg: &std::ffi::OsStr) -> String {
    arg.to_string_lossy().into_owned()
}

/// The path of the file name `name`: the original bytes of an argument not valid UTF-8.
fn raw_path(name: &str) -> &std::path::Path {
    match RAW_ARGS.get().and_then(|raw| raw.get(name)) {
        Some(arg) => std::path::Path::new(arg),
        None => std::path::Path::new(name),
    }
}

/// The program operand when it names an existing `.awk` (or Markdown) file: `zawk lint` reads it
/// as `-f`, and a run that fails to parse it suggests `-f`.
fn program_operand_file(p: &str) -> bool {
    (p.ends_with(".awk") || awk_util::is_markdown_file(p)) && std::path::Path::new(p).is_file()
}

/// A program operand naming a file (see `program_operand_file`), for the hint of a syntax error.
static PROGRAM_OPERAND_FILE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Exits on the syntax error `msg`, suggesting `-f` when the program operand names a file.
fn syntax_failed(msg: &str) -> ! {
    if let Some(file) = PROGRAM_OPERAND_FILE.get() {
        eprintln_ignore!("zawk: {}", msg.strip_prefix("zawk: ").unwrap_or(msg));
        fail!("did you mean `-f {}`?", file)
    }
    fail!("{}", msg)
}

/// Exits on an error splicing in the program sources: a source that is not complete is a syntax
/// error, as the parser reports them; a file that cannot be read is fatal.
fn include_failed(e: &str) -> ! {
    if e.starts_with("syntax error") {
        syntax_failed(e)
    }
    fatal!("{}", e)
}

#[derive(Clone)]
struct PreludeScalars {
    fold_regexes: bool,
    parse_header: bool,
    header_per_file: bool,
    header_in_main: bool,
    escaper: Escaper,
    stage: Stage<()>,
    // The function to run instead of the rules of the program, for `zawk test`.
    test_entry: Option<String>,
}

struct RawPrelude {
    argv: Vec<String>,
    var_decs: Vec<String>,
    // `var=value` operands, grouped by the input file they precede (see `split_operands`).
    operand_assigns: Vec<Vec<String>>,
    operand_files: Vec<String>,
    field_sep: Option<String>,
    output_sep: Option<&'static str>,
    output_record_sep: Option<&'static str>,
    scalars: PreludeScalars,
}

struct Prelude<'a> {
    var_decs: Vec<(&'a str, &'a ast::Expr<'a, 'a, &'a str>)>,
    field_sep: Option<&'a [u8]>,
    output_sep: Option<&'a [u8]>,
    output_record_sep: Option<&'a [u8]>,
    argv: Vec<&'a str>,
    operand_assigns: Vec<Vec<(&'a str, &'a ast::Expr<'a, 'a, &'a str>)>>,
    operand_files: Vec<&'a str>,
    scalars: PreludeScalars,
}

// TODO: make file reading lazy
fn open_file_read(f: &str) -> impl io::BufRead + use<> {
    enum LazyReader<F, R> {
        Uninit(F),
        Init(R),
    }

    impl<R, F: FnMut() -> io::Result<R>> LazyReader<F, R> {
        fn delegate<T>(&mut self, next: impl FnOnce(&mut R) -> io::Result<T>) -> io::Result<T> {
            match self {
                LazyReader::Uninit(f) => {
                    *self = LazyReader::Init(f()?);
                    self.delegate(next)
                }
                LazyReader::Init(r) => next(r),
            }
        }
    }

    // TODO: delegate other methods on read.
    impl<R: io::Read, F: FnMut() -> io::Result<R>> io::Read for LazyReader<F, R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.delegate(|r| r.read(buf))
        }
    }

    // A local file, an object of `s3://bucket/key` or an HTTP(S) URL streamed as it downloads
    // (all transparently decompressed), a database table or query as CSV, standard input (the `-`
    // operand), or nothing (skipped operands).
    enum Source {
        File(Box<dyn io::Read + Send>),
        Http(Box<dyn io::Read + Send>),
        Stdin(io::Stdin),
        Empty,
    }

    impl io::Read for Source {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self {
                Source::File(r) => r.read(buf),
                Source::Http(r) => r.read(buf),
                Source::Stdin(r) => r.read(buf),
                Source::Empty => Ok(0),
            }
        }
    }

    let filename = String::from(f);
    BufReader::new(LazyReader::Uninit(move || {
        match filename.as_str() {
            // As in gawk, an empty operand is skipped, and `-` is standard input.
            "" => Ok(Source::Empty),
            "-" => Ok(Source::Stdin(io::stdin())),
            name if runtime::s3::is_s3_url(name) => {
                runtime::decompress::maybe_decompress(name, runtime::s3::S3Reader::new(name))
                    .map(Source::File)
            }
            name if runtime::http::is_http_url(name) => runtime::http::open(name)
                .and_then(|resp| runtime::decompress::maybe_decompress(name, resp))
                .map(Source::Http),
            name if database::is_db_url(name) => Ok(Source::File(Box::new(database::DbCsv::new(
                name.to_string(),
            )))),
            name => {
                if std::fs::metadata(raw_path(name)).is_ok_and(|m| m.is_dir()) {
                    eprintln_ignore!(
                        "zawk: warning: command line argument `{}' is a directory: skipped",
                        name
                    );
                    return Ok(Source::Empty);
                }
                let file = File::open(raw_path(name))
                    .map_err(|e| common::InputOpenError::new_io(name, &e))?;
                runtime::decompress::maybe_decompress(name, file).map(Source::File)
            }
        }
    }))
}

/// With `-pr`, the input files when the workers can read them in ranges themselves (see
/// `splitter::pread`): all of them local regular files, not compressed. Otherwise, or with
/// `--utf8` (checked by the reading thread), `None`: the files are read by one thread.
fn pread_inputs(
    files: &[String],
    exec_strategy: ExecutionStrategy,
    check_utf8: bool,
) -> Option<Vec<runtime::splitter::pread::PreadFile>> {
    if !matches!(exec_strategy, ExecutionStrategy::ShardPerRecord(_)) || check_utf8 {
        return None;
    }
    let local = |f: &str| {
        !f.is_empty()
            && f != "-"
            && !runtime::s3::is_s3_url(f)
            && !runtime::http::is_http_url(f)
            && !database::is_db_url(f)
            && !xlsx::is_xlsx_file(f)
    };
    if files.is_empty() || !files.iter().all(|f| local(f)) {
        return None;
    }
    runtime::splitter::pread::pread_inputs(files.iter().map(|f| (raw_path(f), f.as_str())))
}

/// Maps `<user>/<repo>/<awk_file_path>` (the part after `gh:`) to its raw GitHub URL on the
/// default branch, e.g. `https://raw.githubusercontent.com/<user>/<repo>/HEAD/<awk_file_path>`.
fn github_raw_url(gh_path: &str) -> Option<String> {
    let mut parts = gh_path.splitn(3, '/');
    let user = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    let file = parts
        .next()
        .map(|s| s.trim_start_matches('/'))
        .filter(|s| !s.is_empty())?;
    Some(format!(
        "https://raw.githubusercontent.com/{user}/{repo}/HEAD/{file}"
    ))
}

/// The program given as text (the program operand or `-e`): the first awk code block of
/// Markdown text, such as the answer of an LLM (`None` when the text is not Markdown). Markdown
/// without an awk code block is a fatal error, as for a Markdown `-f` file.
fn inline_program(text: &str) -> Option<String> {
    awk_util::program_text(text).unwrap_or_else(|e| fatal!("can't read program: {}", e))
}

/// The program in the text of the file `name`: the first awk code block of a Markdown file, else
/// the whole text.
fn program_in_file(name: &str, text: String) -> Result<String, String> {
    if !awk_util::is_markdown_file(name) {
        return Ok(text);
    }
    awk_util::extract_awk_block(&text)
        .ok_or_else(|| "no ```awk code block in the Markdown file".to_string())
}

/// The text of the `-f` file `prog_file`, a local file, a URL or `gh:<user>/<repo>/<path>`, with
/// the local path it was read from. For a Markdown file, it is its first awk code block.
fn read_program_file(prog_file: &str) -> (String, Option<&std::path::Path>) {
    let (text, path) = read_program_source(prog_file);
    match program_in_file(prog_file, text) {
        Ok(text) => (text, path),
        Err(e) => fatal!("can't read program from `{}': {}", prog_file, e),
    }
}

fn read_program_source(prog_file: &str) -> (String, Option<&std::path::Path>) {
    let remote_url = if let Some(gh_path) = prog_file.strip_prefix("gh:") {
        match github_raw_url(gh_path) {
            Some(url) => Some(url),
            None => fatal!(
                "can't open source file `{}' for reading: expected `gh:<user>/<repo>/<awk_file_path>'",
                prog_file
            ),
        }
    } else if prog_file.starts_with("https://") || prog_file.starts_with("http://") {
        Some(prog_file.to_string())
    } else {
        None
    };
    if let Some(url) = remote_url {
        // Error pages (404, proxy errors, ...) must not be run as programs.
        match runtime::network::fetch_text(&url) {
            Ok(p) => (p, None),
            Err(e) => fatal!("can't open source file `{}' for reading: {}", prog_file, e),
        }
    } else {
        match std::fs::read_to_string(raw_path(prog_file)) {
            Ok(p) => (p, Some(std::path::Path::new(prog_file))),
            Err(e) => fatal!(
                "can't open source file `{}' for reading: {}",
                prog_file,
                common::io_error_reason(&e)
            ),
        }
    }
}

/// The smallest total input size that runs in parallel without `-p` (64 MiB, or
/// `ZAWK_PARALLEL_MIN_SIZE` bytes): below it, starting the workers costs more than they save.
const AUTO_PARALLEL_MIN_SIZE: u64 = 64 << 20;

/// Whether automatic parallelism is on: `ZAWK_PARALLEL=0` (or `off`, `false`, `no`) turns it off.
fn auto_parallel_enabled() -> bool {
    !matches!(
        std::env::var("ZAWK_PARALLEL").as_deref().map(str::trim),
        Ok("0" | "off" | "false" | "no")
    )
}

/// Whether the inputs suit automatic parallelism: plain text, CSV or TSV in local regular files
/// (not stdin, URLs or workbooks) of `AUTO_PARALLEL_MIN_SIZE` bytes or more in all.
fn auto_parallel_inputs(input_files: &[String], format: Option<&str>) -> bool {
    if input_files.is_empty() || !matches!(format, None | Some("csv") | Some("tsv")) {
        return false;
    }
    let min = std::env::var("ZAWK_PARALLEL_MIN_SIZE")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(AUTO_PARALLEL_MIN_SIZE);
    let mut total = 0u64;
    for file in input_files {
        if file == "-" || file.contains("://") || xlsx::is_xlsx_file(file) {
            return false;
        }
        match std::fs::metadata(file) {
            Ok(m) if m.is_file() => total += m.len(),
            _ => return false,
        }
    }
    total >= min
}

/// The regex splitter reads its input on one thread: say so when `-p` asked for workers
/// instead of running serially without a word. Only a record separator of several bytes (or one
/// that may be assigned at run time) needs it: any field separator is split in the workers.
fn warn_regex_splitter(strategy: ExecutionStrategy, record_sep: Option<&[u8]>) {
    let flag = match strategy {
        ExecutionStrategy::Serial => return,
        ExecutionStrategy::ShardPerRecord(_) => "r",
        ExecutionStrategy::ShardPerFile => "f",
    };
    let reason = match record_sep {
        Some(rs) => format!(
            "record separator '{}'",
            String::from_utf8_lossy(rs).escape_debug()
        ),
        None => String::from("a separator assigned at run time"),
    };
    eprintln!("zawk: warning: -p {flag} ignored: {reason} needs the regex splitter");
}

fn chained<LR: LineReader>(lr: LR) -> ChainedReader<LR> {
    ChainedReader::new(once(lr))
}

fn get_vars<'a, 'b>(
    vars: impl Iterator<Item = &'b str>,
    a: &'a Arena,
    buf: &mut Vec<u8>,
) -> Vec<(&'a str, &'a ast::Expr<'a, 'a, &'a str>)> {
    let mut res = Vec::new();
    let mut split_buf = Vec::new();
    for var in vars {
        buf.clear();
        split_buf.clear();
        split_buf.extend(var.splitn(2, '='));
        if split_buf.len() != 2 {
            fail!("received -v flag without an '=' sign: {}", var);
        }
        let ident = a.alloc_str(split_buf[0].trim());
        if !lexer::is_ident(ident) {
            fatal!(
                "invalid identifier for left-hand side of -v flag: {}",
                ident
            );
        }
        if lexer::is_keyword(ident) {
            fatal!(
                "cannot assign to `{}` from the command line: it is a reserved word",
                ident
            );
        }
        let str_lit = lexer::parse_string_literal(split_buf[1], a, buf);
        let val = a.alloc(ast::Expr::StrLit(str_lit));
        // -v values of user variables are "strnums", like input. Special variables (FS, RS, ...)
        // keep plain string literals, which lets the reader specialize on constant separators.
        let val = if builtins::Variable::try_from(ident).is_ok() {
            val
        } else {
            a.alloc(ast::Expr::Call(
                common::Either::Right(builtins::Function::Strnum),
                a.alloc_slice(&[val]),
            ))
        };
        res.push((ident, val))
    }
    res
}

fn get_prelude<'a>(a: &'a Arena, raw: &RawPrelude) -> Prelude<'a> {
    let mut buf = Vec::new();
    let output_sep = raw
        .output_sep
        .map(|s| lexer::parse_string_literal(s, a, &mut buf));
    let output_record_sep = raw
        .output_record_sep
        .map(|s| lexer::parse_string_literal(s, a, &mut buf));
    let field_sep = raw
        .field_sep
        .as_ref()
        .map(|s| lexer::parse_string_literal(s.as_str(), a, &mut buf));
    Prelude {
        field_sep,
        var_decs: get_vars(raw.var_decs.iter().map(|s| s.as_str()), a, &mut buf),
        scalars: raw.scalars.clone(),
        output_sep,
        output_record_sep,
        argv: raw.argv.iter().map(|s| a.alloc_str(s.as_str())).collect(),
        operand_assigns: raw
            .operand_assigns
            .iter()
            .map(|group| get_vars(group.iter().map(|s| s.as_str()), a, &mut buf))
            .collect(),
        operand_files: raw
            .operand_files
            .iter()
            .map(|s| a.alloc_str(s.as_str()))
            .collect(),
    }
}

/// Whether a command-line operand is a `var=value` assignment rather than an input file: it
/// starts with an identifier followed by `=`, and no file of that name exists.
fn is_operand_assignment(operand: &str) -> bool {
    match operand.find('=') {
        Some(i) if i > 0 && lexer::is_ident(&operand[..i]) => {
            !std::path::Path::new(operand).exists()
        }
        _ => false,
    }
}

/// Splits command-line operands into input files and `var=value` assignments. The assignments
/// are grouped by the file they precede: group `k` runs before the k-th file is read, and the
/// last group after the last file. Without assignments, no groups are returned.
fn split_operands(operands: &[String]) -> (Vec<String>, Vec<Vec<String>>) {
    if !operands.iter().any(|op| is_operand_assignment(op)) {
        return (operands.to_vec(), Vec::new());
    }
    let mut files = Vec::new();
    let mut groups = vec![Vec::new()];
    for op in operands {
        if is_operand_assignment(op) {
            groups.last_mut().unwrap().push(op.clone());
        } else {
            files.push(op.clone());
            groups.push(Vec::new());
        }
    }
    (files, groups)
}

fn get_context<'a>(
    prog: &str,
    a: &'a Arena,
    mut prelude: Prelude<'a>,
) -> cfg::ProgramContext<'a, &'a str> {
    let text: &'a str = a.alloc_str(prog);
    let lexer = lexer::Tokenizer::new(text);
    let mut buf = Vec::new();
    let parser = parsing::syntax::ProgParser::new();
    let mut prog = ast::Prog::from_stage(a, prelude.scalars.stage.clone());
    prog.argv = mem::take(&mut prelude.argv);
    prog.operand_assigns = mem::take(&mut prelude.operand_assigns);
    prog.operand_files = mem::take(&mut prelude.operand_files);
    let stmt = match parser.parse(a, &mut buf, &mut prog, lexer) {
        Ok(()) => {
            parsing::set_rule_spans(mem::take(&mut prog.rule_spans));
            prog.field_sep = prelude.field_sep;
            prog.prelude_vardecs = prelude.var_decs;
            prog.output_sep = prelude.output_sep;
            prog.output_record_sep = prelude.output_record_sep;
            prog.parse_header = prelude.scalars.parse_header;
            prog.header_per_file = prelude.scalars.header_per_file;
            prog.header_in_main = prelude.scalars.header_in_main;
            if let Some(name) = &prelude.scalars.test_entry {
                parsing::run_only_function(name, a, &mut prog);
            }
            a.alloc(prog)
        }
        Err(e) => syntax_failed(&parsing::syntax_error(text, &e)),
    };
    match cfg::ProgramContext::from_prog(a, stmt, prelude.scalars.escaper) {
        Ok(mut ctx) => {
            ctx.fold_regex_constants = prelude.scalars.fold_regexes;
            ctx
        }
        Err(e) if e.is_syntax() => syntax_failed(&parsing::render_error(&e)),
        Err(e) => fatal!("{}", parsing::render_error(&e)),
    }
}

fn run_interp_with_context<'a>(
    mut ctx: cfg::ProgramContext<'a, &'a str>,
    stdin: impl LineReader,
    ff: impl runtime::writers::FileFactory,
    num_workers: usize,
    signal: CancelSignal,
) {
    let rc = {
        let mut interp = match compile::bytecode(&mut ctx, stdin, ff, num_workers) {
            Ok(ctx) => ctx,
            Err(e) => fatal!("{}", parsing::render_error(&e)),
        };
        interp.set_cancel_signal(signal);
        let res = interp.run();
        // Dropping the interpreter flushes pending output, which `fail!` (exiting the process)
        // would otherwise lose.
        drop(interp);
        match res {
            Err(e) => fatal!("{}", parsing::render_error(&e)),
            Ok(0) => return,
            Ok(n) => n,
        }
    };
    std::process::exit(runtime::writers::exit_code(rc));
}

fn run_cranelift_with_context<'a>(
    mut ctx: cfg::ProgramContext<'a, &'a str>,
    stdin: impl IntoRuntime,
    ff: impl runtime::writers::FileFactory,
    cfg: codegen::Config,
    signal: CancelSignal,
) {
    if let Err(e) = compile::run_cranelift(&mut ctx, stdin, ff, cfg, signal) {
        fatal!("{}", parsing::render_error(&e))
    }
}

/// Parse the command line. As in gawk, usage errors exit with code 1 (clap's default is 2).
fn get_matches(app: clap::Command, args: impl IntoIterator<Item = String>) -> clap::ArgMatches {
    match app.try_get_matches_from(args) {
        Ok(matches) => matches,
        // --help and --version are reported as "errors" that exit successfully.
        Err(e) if e.exit_code() == 0 => e.exit(),
        Err(e) => {
            let _ = e.print();
            std::process::exit(1)
        }
    }
}

// Short options that take a value, either attached (`-F,`) or as the next argument.
const SHORT_WITH_VALUE: &str = "efEOvFBoipj";
const LONG_WITH_VALUE: &[&str] = &[
    "--source",
    "--program-file",
    "--exec",
    "--include",
    "--opt-level",
    "--out-file",
    "--input-format",
    "--field-separator",
    "--backend",
    "--output-format",
    "--chunk-size",
    "--test-entry",
];

/// Whether `arg` is an option whose value is the next argument, such as `-v`, `-Hv` or `--exec`.
fn takes_next_value(arg: &str) -> bool {
    if arg.starts_with("--") {
        return LONG_WITH_VALUE.contains(&arg);
    }
    if !arg.starts_with('-') {
        return false;
    }
    arg.char_indices()
        .skip(1)
        .find(|(_, c)| SHORT_WITH_VALUE.contains(*c))
        .is_some_and(|(pos, c)| pos + c.len_utf8() == arg.len())
}

/// The names clap knows on the top-level command: its options and its subcommands.
struct KnownArgs {
    shorts: Vec<char>,
    longs: Vec<String>,
    subcommands: Vec<String>,
}

impl KnownArgs {
    fn of(app: &clap::Command) -> KnownArgs {
        // clap adds `-h/--help` and `-V/--version` itself when the command is built.
        let mut shorts = vec!['h', 'V'];
        let mut longs = vec![String::from("help"), String::from("version")];
        for arg in app.get_arguments() {
            shorts.extend(arg.get_short_and_visible_aliases().unwrap_or_default());
            longs.extend(
                arg.get_long_and_visible_aliases()
                    .unwrap_or_default()
                    .into_iter()
                    .map(String::from),
            );
        }
        let subcommands = app
            .get_subcommands()
            .flat_map(|cmd| std::iter::once(cmd.get_name()).chain(cmd.get_all_aliases()))
            .map(String::from)
            .collect();
        KnownArgs {
            shorts,
            longs,
            subcommands,
        }
    }

    /// Whether `arg` (starting with `-`) is a zawk option: `--name`, `--name=value`, or a cluster
    /// of known short flags that may end with one taking a value (`-pr`, `-Hv x=1`, `-F,`).
    fn is_option(&self, arg: &str) -> bool {
        if let Some(long) = arg.strip_prefix("--") {
            let name = long.split_once('=').map_or(long, |(name, _)| name);
            return self.longs.iter().any(|l| l == name);
        }
        for c in arg.chars().skip(1) {
            if !self.shorts.contains(&c) {
                return false;
            }
            if SHORT_WITH_VALUE.contains(c) {
                return true;
            }
        }
        arg.len() > 1
    }
}

/// Insert `--` where the options of the command line end, so that clap treats all the remaining
/// arguments as operands for `ARGV`:
///
/// * As in gawk, `-E file` is the last option: even `-v` or `--help` after it are the script's
///   own. This is what makes `#!/usr/bin/env -S zawk -E` scripts receive their arguments untouched.
/// * After the first operand (the program text, or with `-f` the first input file), zawk's own
///   options are still accepted (`zawk '{...}' data.csv -i csv`), but the first argument that is not
///   one ends the options, as in POSIX awk: `zawk 'BEGIN { print ARGV[1] }' -x` prints `-x`, and
///   `zawk -f script.awk data -n 5` passes `-n` and `5` to the script.
///
/// A subcommand name (`zawk lint ...`) before any operand is left to clap.
fn end_options(mut args: Vec<String>, known: &KnownArgs) -> Vec<String> {
    let mut seen_operand = false;
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            return args;
        }
        if arg == "-" || !arg.starts_with('-') {
            if !seen_operand && known.subcommands.iter().any(|name| name == arg) {
                return args;
            }
            seen_operand = true;
            i += 1;
            continue;
        }
        if seen_operand && !known.is_option(arg) {
            args.insert(i, String::from("--"));
            return args;
        }
        // The index of the `-E` value, if this argument is `-E`.
        let mut exec_value = None;
        let mut skip_next = false;
        if let Some(long) = arg.strip_prefix("--") {
            if long == "exec" {
                exec_value = Some(i + 1);
            } else if long.starts_with("exec=") {
                exec_value = Some(i);
            } else if LONG_WITH_VALUE.contains(&arg) {
                skip_next = true;
            }
        } else {
            for (pos, c) in arg.char_indices().skip(1) {
                if SHORT_WITH_VALUE.contains(c) {
                    let attached = pos + c.len_utf8() < arg.len();
                    if c == 'E' {
                        exec_value = Some(if attached { i } else { i + 1 });
                    } else {
                        skip_next = !attached;
                    }
                    break;
                }
            }
        }
        if let Some(v) = exec_value {
            if v + 1 < args.len() && args[v + 1] != "--" {
                args.insert(v + 1, String::from("--"));
            }
            return args;
        }
        i += if skip_next { 2 } else { 1 };
    }
    args
}

/// `zawk llm`: prints the prompt. Exits with 2 if an input file can't be read.
fn run_llm(matches: &clap::ArgMatches) -> ! {
    let mut files: Vec<String> = matches
        .get_many::<String>("args")
        .unwrap()
        .cloned()
        .collect();
    // The question is the last argument, unless it is an input: `-`, a URL or S3 object, or an
    // existing file (`book.xlsx#Sheet2` selects a sheet). A question is never empty.
    let is_input = |arg: &str| {
        !arg.chars().any(char::is_whitespace)
            && !arg.is_empty()
            && (arg == "-"
                || arg.contains("://")
                || std::path::Path::new(arg).exists()
                || arg
                    .rsplit_once('#')
                    .is_some_and(|(path, _)| std::path::Path::new(path).exists()))
    };
    let question = match files.last() {
        Some(last) if is_input(last) => None,
        _ => files.pop(),
    };
    if question.as_ref().is_some_and(|q| q.trim().is_empty()) {
        eprintln!("zawk: the question (the last argument) is empty");
        std::process::exit(2);
    }
    // `-F` with escapes processed as for a run.
    let field_sep = matches.get_one::<String>("field-separator").map(|fs| {
        let a = Arena::default();
        let mut buf = Vec::new();
        (
            fs.clone(),
            String::from_utf8_lossy(lexer::parse_string_literal(fs, &a, &mut buf)).into_owned(),
        )
    });
    let opts = llm::Options {
        input_format: matches.get_one::<String>("input-format").cloned(),
        header: matches.get_flag("parse-header"),
        field_sep,
        output_format: matches.get_one::<String>("output-format").cloned(),
        sample_rows: *matches.get_one::<usize>("rows").unwrap(),
        max_rows: matches.get_one::<u64>("max-rows").copied(),
        files,
        question,
    };
    match llm::prompt(&opts) {
        Ok(prompt) => {
            // The prompt is often piped: a closed pipe is not an error.
            let mut out = io::stdout().lock();
            let _ = out.write_all(prompt.as_bytes()).and_then(|_| out.flush());
            std::process::exit(0);
        }
        Err(e) => {
            eprintln!("zawk: {}", e);
            std::process::exit(2);
        }
    }
}

/// `zawk fmt`: prints the formatted programs, writes them back with `-w`, or with `--check` exits
/// with 1 if a program is not formatted. Exits with 2 if a program can't be read or has a syntax
/// error.
fn run_fmt(matches: &clap::ArgMatches) -> ! {
    let opts = fmt::Options {
        indent: if matches.get_flag("tabs") {
            "\t".to_string()
        } else {
            " ".repeat(*matches.get_one::<usize>("indent").unwrap())
        },
    };
    let write = matches.get_flag("write");
    let check = matches.get_flag("check");
    // (name, text); the name is None for stdin and `-e`.
    let mut programs: Vec<(Option<String>, String)> = Vec::new();
    if let Some(text) = matches.get_one::<String>("source") {
        programs.push((None, text.clone()));
    } else {
        let files: Vec<String> = matches
            .get_many::<String>("files")
            .map(|x| x.cloned().collect())
            .unwrap_or_else(|| vec!["-".to_string()]);
        for file in files {
            if awk_util::is_markdown_file(&file) {
                eprintln!("zawk: {}: formatting Markdown files is not supported", file);
                std::process::exit(2)
            }
            let text = if file == "-" {
                io::read_to_string(io::stdin())
            } else {
                std::fs::read_to_string(&file)
            };
            match text {
                Ok(text) => programs.push(((file != "-").then_some(file), text)),
                Err(e) => {
                    eprintln!("zawk: can't read {}: {}", file, common::io_error_reason(&e));
                    std::process::exit(2)
                }
            }
        }
    }
    let mut failed = false;
    let mut unformatted = false;
    for (name, text) in &programs {
        let label = name.as_deref().unwrap_or(if matches.contains_id("source") {
            "-e"
        } else {
            "<stdin>"
        });
        let out = match fmt::format(text, &opts) {
            Ok(out) => out,
            Err(e) => {
                eprintln!("zawk: {}: {}", label, e);
                failed = true;
                continue;
            }
        };
        if check {
            if out != *text {
                unformatted = true;
                println!("{}", label);
            }
        } else if let (true, Some(file)) = (write, name) {
            if out != *text
                && let Err(e) = std::fs::write(file, &out) {
                    eprintln!(
                        "zawk: can't write {}: {}",
                        file,
                        common::io_error_reason(&e)
                    );
                    failed = true;
                }
        } else {
            print!("{}", out);
        }
    }
    std::process::exit(if failed {
        2
    } else if unformatted {
        1
    } else {
        0
    })
}

/// A program source given on the command line.
#[derive(Clone, Copy, PartialEq)]
enum SourceKind {
    /// `-f` or `-E`: a program file.
    File,
    /// `-e`: a program text.
    Inline,
    /// `--include`: a library file, spliced in as `@include` does.
    Include,
}

/// `zawk lint`: prints the JSON report and exits with 1 if the program has errors (or warnings,
/// with `--deny-warnings`) or a program file cannot be read.
fn run_lint(matches: &clap::ArgMatches) -> ! {
    // The sources, with whether each is a local file (`-f`), a program text (`-e`) or an
    // `--include` file (whose name is the text).
    let mut sources: Vec<(usize, lint::Source, SourceKind)> = Vec::new();
    if let (Some(idx), Some(vals)) = (
        matches.indices_of("program-file"),
        matches.get_many::<String>("program-file"),
    ) {
        for (i, file) in idx.zip(vals) {
            let (text, kind) = if file == "-" {
                (io::read_to_string(io::stdin()), SourceKind::Inline)
            } else {
                (std::fs::read_to_string(file), SourceKind::File)
            };
            match text
                .map_err(|e| common::io_error_reason(&e))
                .and_then(|text| program_in_file(file, text))
            {
                Ok(text) => sources.push((
                    i,
                    lint::Source {
                        name: Some(file.clone()),
                        text,
                        line: 0,
                    },
                    kind,
                )),
                Err(e) => {
                    println!("{}", lint::file_error(file, &e));
                    std::process::exit(1)
                }
            }
        }
    }
    if let (Some(idx), Some(vals)) = (
        matches.indices_of("source"),
        matches.get_many::<String>("source"),
    ) {
        sources.extend(idx.zip(vals).map(|(i, text)| {
            let text = inline_program(text).unwrap_or_else(|| text.clone());
            (
                i,
                lint::Source {
                    name: None,
                    text,
                    line: 0,
                },
                SourceKind::Inline,
            )
        }));
    }
    if let (Some(idx), Some(vals)) = (
        matches.indices_of("include"),
        matches.get_many::<String>("include"),
    ) {
        sources.extend(idx.zip(vals).map(|(i, file)| {
            (
                i,
                lint::Source {
                    name: None,
                    text: file.clone(),
                    line: 0,
                },
                SourceKind::Include,
            )
        }));
    }
    sources.sort_by_key(|(i, _, _)| *i);
    let mut sources: Vec<(lint::Source, SourceKind)> =
        sources.into_iter().map(|(_, s, k)| (s, k)).collect();
    let program_sources = sources
        .iter()
        .filter(|(_, k)| *k != SourceKind::Include)
        .count();
    // As for a run, `-e` sources are named `-e#N` when the program has more than one source.
    if program_sources > 1 {
        let mut inline_count = 0;
        for (src, _) in sources
            .iter_mut()
            .filter(|(s, k)| s.name.is_none() && *k != SourceKind::Include)
        {
            inline_count += 1;
            src.name = Some(parsing::inline_source_name(inline_count));
        }
    }
    let mut input_files: Vec<String> = matches
        .get_many::<String>("input-files")
        .map(|x| x.cloned().collect())
        .unwrap_or_default();
    if program_sources > 0 {
        // As for a run, with -f or -e the program operand is the first input file.
        if let Some(p) = matches.get_one::<String>("program") {
            input_files.insert(0, p.clone());
        }
    } else if let Some(file) = matches
        .get_one::<String>("program")
        .filter(|p| program_operand_file(p))
    {
        // `zawk lint prog.awk data.csv`: a program operand naming an `.awk` (or Markdown) file is
        // read as -f.
        match std::fs::read_to_string(file)
            .map_err(|e| common::io_error_reason(&e))
            .and_then(|text| program_in_file(file, text))
        {
            Ok(text) => sources.push((
                lint::Source {
                    name: Some(file.clone()),
                    text,
                    line: 0,
                },
                SourceKind::File,
            )),
            Err(e) => {
                println!("{}", lint::file_error(file, &e));
                std::process::exit(1)
            }
        }
    }
    if sources.iter().all(|(_, k)| *k == SourceKind::Include) {
        let text = match matches.get_one::<String>("program") {
            // As for a run, sugar syntax only applies to a program operand.
            Some(p) => match inline_program(p) {
                Some(code) if matches.get_flag("posix") => code,
                Some(code) => awk_util::sugar_syntax_convert(code),
                None if matches.get_flag("posix") => p.clone(),
                None => awk_util::sugar_syntax_convert(p.trim().to_string()),
            },
            None => match io::read_to_string(io::stdin()) {
                Ok(text) => inline_program(&text).unwrap_or(text),
                Err(e) => fatal!("can't read program from stdin: {}", e),
            },
        };
        sources.push((
            lint::Source {
                name: None,
                text,
                line: 0,
            },
            SourceKind::Inline,
        ));
    }
    // Splice in the included files, as for a run.
    let mut includer = include::Includer::default();
    for (src, kind) in sources {
        let res = match kind {
            SourceKind::Include => includer.include(&src.text),
            SourceKind::File => {
                let path = src.name.as_deref().map(std::path::Path::new);
                includer.add_source(src.name.clone(), &src.text, path)
            }
            SourceKind::Inline => includer.add_source(src.name, &src.text, None),
        };
        if let Err(e) = res {
            println!("{}", lint::include_error(&e));
            std::process::exit(1)
        }
    }
    let sources = includer.pieces;
    let format = input_format(matches, &input_files);
    let format = format.as_deref();
    let mut opts = lint_options(matches, &input_files, format);
    // The data-aware checks read the first records of the first input file.
    // Operand assignments (`OFS=,`) are not files, so skip them.
    if let Some(file) = input_files
        .iter()
        .find(|f| !is_operand_assignment(f))
        .filter(|_| !matches!(format, Some("markdown" | "md")))
    {
        let rows = *matches.get_one::<usize>("sample-rows").unwrap();
        match inspect::sample(
            file,
            format.and_then(inspect::Format::parse),
            opts.header,
            rows,
        ) {
            // a database password is not shown in the diagnostics
            Ok(sample) => {
                opts.data = Some(lint::Data {
                    file: runtime::network::redact_url(file).into_owned(),
                    sample,
                })
            }
            Err(e) => {
                println!(
                    "{}",
                    lint::input_error(
                        &runtime::network::redact_url(file),
                        &common::io_error_reason(&e)
                    )
                );
                std::process::exit(1)
            }
        }
    }
    let (errors, warnings, report) = lint::lint(&sources, &opts);
    println!("{}", report);
    let failed = errors > 0 || (warnings > 0 && matches.get_flag("deny-warnings"));
    std::process::exit(if failed { 1 } else { 0 })
}

/// `zawk --lint`: prints the JSON report to stderr when the program has diagnostics, and exits
/// with 1 without running the program when it has errors (or warnings, with `--deny-warnings`).
fn lint_before_run(
    sources: &[lint::Source],
    matches: &clap::ArgMatches,
    input_files: &[String],
    format: Option<&str>,
) {
    let opts = lint_options(matches, input_files, format);
    let (errors, warnings, report) = lint::lint(sources, &opts);
    if errors + warnings > 0 {
        eprintln!("{}", report);
    }
    if errors > 0 || (warnings > 0 && matches.get_flag("deny-warnings")) {
        std::process::exit(1)
    }
}

/// Warns about each Parquet or Avro input file (the extension and the magic both match) that the
/// run does not read in its own format, such as `zawk '...' demo.avro in.txt`: the binary file
/// would otherwise be split into lines of garbage without any notice.
fn warn_binary_read_as_text(format: Option<&str>, input_files: &[String]) {
    for file in input_files.iter().filter(|f| !is_operand_assignment(f)) {
        let kind = if format != Some("parquet") && parquet::is_parquet_file(file) {
            "parquet"
        } else if format != Some("avro") && avro::is_avro_file(file) {
            "avro"
        } else {
            continue;
        };
        eprintln!(
            "zawk: warning: `{}' is a binary {} file but is read as {}; use -i {} with only {} input files",
            file,
            kind,
            format.unwrap_or("text"),
            kind,
            kind
        );
    }
}

/// The input format of a run (or of `zawk lint`): `-i`, or, when it is not given, `parquet` when
/// every input file is a Parquet file (`.parquet` that starts with `PAR1`), `avro` when every
/// input file is an Avro file (`.avro` that starts with `Obj\x01`), and, unless a field
/// separator is given, `jsonl` when every input file ends with `.jsonl` or `.ndjson` and `json`
/// when every input file is a JSON document (`.json` that starts with `{` or `[`).
fn input_format(matches: &clap::ArgMatches, input_files: &[String]) -> Option<String> {
    if let Some(format) = matches.get_one::<String>("input-format") {
        return Some(format.clone());
    }
    let field_sep = matches
        .try_get_one::<String>("field-separator")
        .ok()
        .flatten()
        .is_some()
        || matches
            .try_get_many::<String>("var")
            .ok()
            .flatten()
            .into_iter()
            .flatten()
            .any(|v| v.trim_start().starts_with("FS="));
    let files: Vec<&String> = input_files
        .iter()
        .filter(|f| !is_operand_assignment(f))
        .collect();
    // Database tables and queries are read as CSV with a header line.
    if files.iter().any(|f| database::is_db_url(f)) {
        return Some(String::from("database"));
    }
    // Excel workbooks (`book.xlsx`, `book.xlsx#Sheet2`) are read as CSV. Mixed with other files,
    // the run is `-i csv`: each workbook is converted to CSV and the other files are read as CSV.
    if files.iter().any(|f| xlsx::is_xlsx_file(f)) {
        let all = files.iter().all(|f| xlsx::is_xlsx_file(f));
        return Some(String::from(if all { "xlsx" } else { "csv" }));
    }
    // Parquet files (`.parquet` starting with `PAR1`) are binary, so a field separator does not
    // matter.
    if !files.is_empty() && files.iter().all(|f| parquet::is_parquet_file(f)) {
        return Some(String::from("parquet"));
    }
    // Avro files (`.avro` starting with `Obj\x01`) are binary too.
    if !files.is_empty() && files.iter().all(|f| avro::is_avro_file(f)) {
        return Some(String::from("avro"));
    }
    if !field_sep
        && !files.is_empty()
        && files
            .iter()
            .all(|f| runtime::splitter::json::is_jsonl_file(f))
    {
        return Some(String::from("jsonl"));
    }
    (!field_sep
        && !files.is_empty()
        && files.iter().all(|f| {
            runtime::splitter::json::is_json_file(f) && runtime::splitter::json::looks_like_json(f)
        }))
    .then(|| String::from("json"))
}

/// The lint options given by the `-p`, `-v` and `-H` arguments of `zawk lint` or
/// of a run, its `var=value` operands, and its input `format`.
fn lint_options(
    matches: &clap::ArgMatches,
    input_files: &[String],
    format: Option<&str>,
) -> lint::Options {
    // As for a run, JSON, JSON Lines, Parquet, Avro, Markdown and database input imply -H.
    let header = matches.get_flag("parse-header")
        || matches!(
            format,
            Some("json" | "jsonl" | "ndjson" | "parquet" | "avro" | "markdown" | "md" | "database")
        );
    let vars = || matches.get_many::<String>("var").into_iter().flatten();
    // `-F`, or `-v FS=...`, with escapes processed as for a run.
    let field_sep = matches
        .try_get_one::<String>("field-separator")
        .ok()
        .flatten()
        .cloned()
        .or_else(|| vars().find_map(|v| v.strip_prefix("FS=").map(String::from)))
        .map(|fs| {
            let a = Arena::default();
            let mut buf = Vec::new();
            String::from_utf8_lossy(lexer::parse_string_literal(&fs, &a, &mut buf)).into_owned()
        });
    lint::Options {
        header,
        field_sep,
        data: None,
        parallel: match matches
            .get_one::<String>("parallel-strategy")
            .map(|s| s.as_str())
        {
            Some("r") | Some("record") => Some(lint::Parallel::Record),
            Some("f") | Some("file") => Some(lint::Parallel::File),
            // `--keep-order` alone runs as `-pr --keep-order` (`zawk lint` has no such flag).
            _ if matches!(matches.try_get_one::<bool>("keep-order"), Ok(Some(true))) => {
                Some(lint::Parallel::Record)
            }
            _ => None,
        },
        // `-v var=value` and `var=value` operands both set `var` before it is read.
        assigned: vars()
            .chain(split_operands(input_files).1.iter().flatten())
            .map(|v| {
                v.split_once('=')
                    .map_or(v.as_str(), |(n, _)| n)
                    .trim()
                    .to_string()
            })
            .collect(),
    }
}

const DEFAULT_OPT_LEVEL: i32 = 3;

/// The columns of Parquet input that the program reads (see `parquet::Projection`). The program
/// is analyzed on its own here, as the input reader is created before the program is compiled.
fn parquet_projection(
    prog: &str,
    raw: &RawPrelude,
    fi_names: &[String],
) -> Option<Arc<parquet::Projection>> {
    let a = Arena::default();
    let mut ctx = get_context(prog, &a, get_prelude(&a, raw));
    // An error is reported when the program is compiled to run.
    let (fields, names) = compile::input_projection(&mut ctx).ok()?;
    Some(Arc::new(parquet::Projection::new(
        fields,
        names,
        fi_names.to_vec(),
    )))
}

fn dump_bytecode(prog: &str, raw: &RawPrelude) -> String {
    use std::io::Cursor;
    let a = Arena::default();
    let mut ctx = get_context(prog, &a, get_prelude(&a, raw));
    let fake_inp: Box<dyn io::Read + Send> = Box::new(Cursor::new(vec![]));
    let interp = match compile::bytecode(
        &mut ctx,
        chained(CSVReader::new(
            once((fake_inp, String::from("unused"))),
            InputFormat::CSV,
            CHUNK_SIZE,
            /*check_utf8=*/ false,
            ExecutionStrategy::Serial,
            Default::default(),
        )),
        runtime::writers::default_factory(),
        /*num_workers=*/ 1,
    ) {
        Ok(ctx) => ctx,
        Err(e) => fatal!("{}", parsing::render_error(&e)),
    };
    let mut v = Vec::<u8>::new();
    for (i, func) in interp.instrs().iter().enumerate() {
        writeln!(&mut v, "function {} {{", i).unwrap();
        for (j, inst) in func.iter().enumerate() {
            writeln!(&mut v, "\t[{:2}] {:?}", j, inst).unwrap();
        }
        writeln!(&mut v, "}}\n").unwrap();
    }
    String::from_utf8(v).unwrap()
}

fn main() {
    // Run on a thread with a large stack: the cranelift backend recurses on the native stack.
    common::run_with_program_stack(real_main)
}

fn real_main() {
    let start = std::time::Instant::now();
    //.env load support
    dotenvy::dotenv().ok();
    runtime::logging::init();
    runtime::tls::install_default_provider();
    let dump_cmd = Command::new("dump")
        .about("Dump text to CSV (requires --prometheus)")
        .arg(
            Arg::new("prometheus")
                .long("prometheus")
                .num_args(0)
                .help("Parse Prometheus metrics to CSV"),
        )
        .arg(
            Arg::new("input-file")
                .index(1)
                .required(true)
                .help("Text file or URL to parse"),
        );
    let init_cmd = Command::new("init")
        .about("Create a new AWK file with help info")
        .arg(
            Arg::new("awk-file")
                .index(1)
                .required(true)
                .help("AWK file to create"),
        );
    let functions_cmd = Command::new("functions").about("List builtin functions with signatures, descriptions and examples")
        .arg(Arg::new("json")
            .long("json")
            .num_args(0)
            .help("Output as NDJSON, one function per line: name, category, signature, params, returns, summary and example")
        )
        .arg(Arg::new("filter")
            .index(1)
            .help("Only list functions whose name contains FILTER, or whose category is FILTER")
        );
    let lint_cmd = Command::new("lint")
        .about("Check an AWK program for errors without running it, and report them as JSON")
        .arg(Arg::new("program-file")
            .short('f')
            .long("program-file")
            .num_args(1)
            .action(clap::ArgAction::Append)
            .help("Lint the program in this file (can be repeated)"))
        .arg(Arg::new("source")
            .short('e')
            .long("source")
            .num_args(1)
            .action(clap::ArgAction::Append)
            .help("Lint this program text (can be repeated, combined with -f in order)"))
        .arg(Arg::new("include")
            .long("include")
            .num_args(1)
            .action(clap::ArgAction::Append)
            .value_name("file")
            .help("Include this awk library file, as `@include \"file\"` does (can be repeated)"))
        .arg(Arg::new("posix")
            .long("posix")
            .num_args(0)
            .help("Do not expand the sugar syntax of a program operand (`$1,$2` or `/re/`)"))
        .arg(Arg::new("var")
            .short('v')
            .num_args(1)
            .action(clap::ArgAction::Append)
            .help("A variable the run sets with `-v var=value` (it is not reported as never set)"))
        .arg(Arg::new("parallel-strategy")
            .short('p')
            .value_parser(["r", "record", "f", "file"])
            .help("Also report code whose meaning changes when it runs in parallel with `-pr` or `-pf`"))
        .arg(Arg::new("parse-header")
            .short('H')
            .long("parse-header")
            .num_args(0)
            .help("The run uses -H: `FI` maps the column names of the header (`$FI[...]` is not reported)"))
        .arg(Arg::new("input-format")
            .short('i')
            .long("input-format")
            .value_parser(["csv", "tsv", "json", "jsonl", "ndjson", "parquet", "avro", "markdown", "md", "xlsx"])
            .help("The input format of the run (json for `.json`, jsonl for `.jsonl`/`.ndjson`, parquet for `.parquet`, avro for `.avro` and xlsx for `.xlsx` files when not given); json, jsonl, parquet, avro and markdown imply -H"))
        .arg(Arg::new("field-separator")
            .short('F')
            .long("field-separator")
            .num_args(1)
            .value_name("FS")
            .conflicts_with("input-format")
            .help("The field separator of the run, to split the records of the input file"))
        .arg(Arg::new("sample-rows")
            .long("sample-rows")
            .value_parser(clap::value_parser!(usize))
            .default_value("100")
            .help("The number of records of the input file the data-aware checks read"))
        .arg(Arg::new("deny-warnings")
            .long("deny-warnings")
            .num_args(0)
            .help("Exit with 1 when there are warnings, as when there are errors"))
        .arg(Arg::new("program")
            .help("The program text (or an `.awk` file) to lint; read from stdin when no program is given"))
        .arg(Arg::new("input-files")
            .num_args(1..)
            .help("The input files of the run: the program is also checked against the first records of the first one (unknown `$FI[...]` columns, fields beyond NF, a wrong field separator, number comparisons of fields that are not numbers)"));
    let fmt_cmd = Command::new("fmt")
        .about("Format AWK programs: indent blocks, one statement per line, normalized spaces; comments are kept")
        .arg(Arg::new("source")
            .short('e')
            .long("source")
            .num_args(1)
            .conflicts_with("files")
            .help("Format this program text"))
        .arg(Arg::new("write")
            .short('w')
            .long("write")
            .num_args(0)
            .conflicts_with("check")
            .help("Write the result to the files instead of standard output"))
        .arg(Arg::new("check")
            .long("check")
            .num_args(0)
            .help("Only list the programs that are not formatted, and exit with 1 if there are any"))
        .arg(Arg::new("indent")
            .long("indent")
            .value_parser(clap::value_parser!(usize))
            .default_value("4")
            .help("The number of spaces of one level of indentation"))
        .arg(Arg::new("tabs")
            .long("tabs")
            .num_args(0)
            .help("Indent with tabs instead of spaces"))
        .arg(Arg::new("files")
            .num_args(1..)
            .help("The program files to format (`-` for standard input); read from stdin when no file is given"));
    let test_cmd = Command::new("test")
        .about(
            "Run the test_* functions of AWK files: each runs alone, without the rules of its file",
        )
        .arg(
            Arg::new("paths")
                .num_args(0..)
                .value_name("path")
                .help("AWK files, or directories to search for .awk files [default: .]"),
        )
        .arg(
            Arg::new("filter")
                .short('k')
                .long("filter")
                .num_args(1)
                .value_name("text")
                .help("Only run the tests whose function name contains this text"),
        );
    let inspect_cmd = Command::new("inspect")
        .about("Show the schema, the first rows (and the last 10 rows of an input with more than 50 rows) and summary statistics of a CSV, TSV, JSON Lines, Parquet, Avro or Excel (xlsx) file, or of a database table or query (`sqlite://app.db#user`)")
        .arg(Arg::new("input-format")
            .short('i')
            .long("input-format")
            .value_parser(["csv", "tsv", "json", "jsonl", "ndjson", "parquet", "avro", "xlsx"])
            .help("The format of the files; by default it is guessed from the file extension, then from the content"))
        .arg(Arg::new("output-format")
            .short('o')
            .long("output-format")
            .value_parser(["table", "markdown", "md"])
            .default_value("table")
            .help("Print the report as monospaced tables (table) or as Markdown with Markdown tables (markdown)"))
        .arg(Arg::new("rows")
            .short('n')
            .long("rows")
            .value_parser(clap::value_parser!(usize))
            .default_value("10")
            .help("The number of sample rows"))
        .arg(Arg::new("max-rows")
            .long("max-rows")
            .value_parser(clap::value_parser!(u64))
            .help("Only read the first MAX-ROWS rows (the statistics are computed on them)"))
        .arg(Arg::new("files")
            .num_args(1..)
            .required(true)
            .help("Files, URLs or S3 objects to inspect (`-` for standard input)"));
    let llm_cmd = Command::new("llm")
        .about("Print a prompt asking an LLM to write a zawk program that answers a question about the input files, e.g. `zawk llm -i csv -H family.csv 'how many adults?' | llm`")
        .arg(Arg::new("input-format")
            .short('i')
            .long("input-format")
            .conflicts_with("field-separator")
            .value_parser(["csv", "tsv", "json", "jsonl", "ndjson", "parquet", "avro", "markdown", "md", "xlsx"])
            .help("The input format of the run; text lines split by FS when not given (json when every input file ends with .json, xlsx when every one ends with .xlsx)"))
        .arg(Arg::new("parse-header")
            .short('H')
            .long("parse-header")
            .num_args(0)
            .help("The run uses -H: the first line is the header, and `FI` maps column names to field numbers"))
        .arg(Arg::new("field-separator")
            .short('F')
            .long("field-separator")
            .num_args(1)
            .value_name("FS")
            .help("The field separator of the run"))
        .arg(Arg::new("output-format")
            .short('o')
            .long("output-format")
            .value_parser(["csv", "tsv", "jsonl", "ndjson", "markdown", "md", "table", "parquet", "avro", "xlsx"])
            .help("The output format of the run"))
        .arg(Arg::new("rows")
            .short('n')
            .long("rows")
            .value_parser(clap::value_parser!(usize))
            .default_value("10")
            .help("The number of sample lines of each input file"))
        .arg(Arg::new("max-rows")
            .long("max-rows")
            .value_parser(clap::value_parser!(u64))
            .default_value("100000")
            .help("Only read the first MAX-ROWS rows of each input file for the summary"))
        .arg(Arg::new("args")
            .num_args(1..)
            .required(true)
            .value_name("FILES... [QUESTION]")
            .help("The input files (files, URLs or S3 objects), then the question as the last argument; without a question, the prompt has no Question section"));
    let app = Command::new("zawk")
        .version(builtins::VERSION)
        .author("Eli R, linux_china")
        .about("zawk is an AWK language implementation by Rust with stdlib support")
        .subcommand(dump_cmd)
        .subcommand(init_cmd)
        .subcommand(functions_cmd)
        .subcommand(lint_cmd)
        .subcommand(inspect_cmd)
        .subcommand(fmt_cmd)
        .subcommand(test_cmd)
        .subcommand(llm_cmd)
        .arg(Arg::new("program-file")
            .long("program-file")
            .short('f')
            .num_args(1)
            .action(clap::ArgAction::Append)
            .help("Read the program source from the file/url program-file, instead of from the command line. Multiple '-f' options may be used"))
        .arg(Arg::new("source")
            .long("source")
            .short('e')
            .num_args(1)
            .action(clap::ArgAction::Append)
            .value_name("program-text")
            .help("Use program-text as program source code. May be mixed with '-f'; all sources are concatenated in command-line order"))
        .arg(Arg::new("exec")
            .long("exec")
            .short('E')
            .num_args(1)
            .value_name("file")
            .help("Like '-f', but this is the last option processed: all following arguments are input files. Intended for `#!/usr/bin/env -S zawk -E` scripts"))
        .arg(Arg::new("include")
            .long("include")
            .num_args(1)
            .action(clap::ArgAction::Append)
            .value_name("file")
            .help("Load the awk library file before the program, as `@include \"file\"` does: searched in AWKPATH (default `.:/usr/local/share/awk`), loaded once. Multiple '--include' options may be used"))
        .arg(Arg::new("characters-as-bytes")
            .long("characters-as-bytes")
            .short('b')
            .num_args(0)
            .help("Treat all input data as single-byte characters: length(), substr(), index(), match() and printf widths count bytes"))
        .arg(Arg::new("posix")
            .long("posix")
            .num_args(0)
            .help("POSIX compatibility: disable zawk's command-line sugar syntax (e.g. `zawk '$1,$2'`, `zawk '/re/'`). zawk extension functions remain available"))
        .arg(Arg::new("test-entry")
            .long("test-entry")
            .num_args(1)
            .hide(true)
            .help("Run this function instead of the rules of the program (used by `zawk test`)"))
        .arg(Arg::new("lint")
            .long("lint")
            .num_args(0)
            .help("Lint the program before running it, as `zawk lint` does: the JSON report is printed to stderr when there are diagnostics, and the program does not run when there are errors"))
        .arg(Arg::new("deny-warnings")
            .long("deny-warnings")
            .num_args(0)
            .help("Implies --lint: the program does not run, and zawk exits with 1, when there are warnings too"))
        .arg(Arg::new("opt-level")
            .long("opt-level")
            .short('O')
            .num_args(1)
            .allow_hyphen_values(true)
            .help("The optimization level for the program. Level `-1` forces bytecode interpretation; level 3 enables constant folding of regular expressions")
            .value_parser(["-1", "0", "1", "2", "3"]))
        .arg(Arg::new("out-file")
            .long("out-file")
            .num_args(1)
            .value_name("FILE")
            .help("Write to specified output file instead of standard output"))
        .arg(Arg::new("utf8")
            .long("utf8")
            .num_args(0)
            .help("Validate all input as UTF-8, returning an error if it is invalid"))
        .arg(Arg::new("dump-cfg")
            .long("dump-cfg")
            .num_args(0)
            .help("Print untyped SSA form for input program"))
        .arg(Arg::new("dump-bytecode")
            .long("dump-bytecode")
            .num_args(0)
            .help("Print bytecode for input program"))
        .arg(Arg::new("parse-header")
            .long("parse-header")
            .short('H')
            .num_args(0)
            .help("Consume the first line of input and populate the `FI` variable with column names mapping to column indexes"))
        .arg(Arg::new("input-format")
            .long("input-format")
            .short('i')
            .value_name("csv|tsv|json|jsonl|parquet|avro|markdown|xlsx")
            .conflicts_with("field-separator")
            .help("Input is split according to the rules of (csv|tsv|json|jsonl|parquet|avro|markdown). $0 contains the unescaped line. Assigning to columns does nothing. jsonl implies -H: `FI` maps JSON keys to columns in the order they are first seen (a key first seen in a later record is appended, so NF grows from that record on). json reads a JSON document as JSON Lines: each element of an array is a record, an object is one record, and a string or number is a record of plain text; `file.json#$.data` selects the records with a JSONPath. json is the default when every input file ends with .json, jsonl when every input file ends with .jsonl or .ndjson, and parquet when every input file ends with .parquet (and starts with the PAR1 magic), and avro when every input file ends with .avro (and starts with the Obj\\x01 magic). parquet implies -H: `FI` maps column names to columns, $0 is the row as a JSON object. avro reads an Avro Object Container File (any codec) and implies -H: `FI` maps the fields of the record schema to columns, $0 is the record as a JSON object; booleans are 1/0, dates YYYY-MM-DD, timestamps RFC 3339 UTC, decimals exact, bytes hex, nested values JSON and null empty (or the --null text). markdown (md) reads the first table as CSV and implies -H: `FI` maps column names (without `:TYPE` suffixes) to columns. xlsx reads a sheet of an Excel workbook as CSV (`book.xlsx#Sheet2` for a sheet, the first one by default); with -H its first row is the header. xlsx is the default when every input file ends with .xlsx (or .xls, .xlsm, .xlsb, .ods); with other input files, or with -i csv/tsv, each workbook is converted to CSV/TSV and the run is -i csv")
            .value_parser(["csv", "tsv", "json", "jsonl", "ndjson", "parquet", "avro", "markdown", "md", "xlsx"]))
        .arg(Arg::new("var")
            .short('v')
            .num_args(1)
            .action(clap::ArgAction::Append)
            .value_name("var=val")
            .help("Assign the value <val> to the variable <var>, before execution of the zawk program begins. Multiple '-v' options may be used"))
        .arg(Arg::new("field-separator")
            .long("field-separator")
            .short('F')
            .num_args(1)
            .value_name("FS")
            .conflicts_with("input-format")
            .help("Field separator `FS` for zawk program"))
        .arg(Arg::new("backend")
            .long("backend")
            .short('B')
            .help("The backend used to run the zawk program, ranging from fastest to compile and slowest to execute, and slowest to compile and fastest to execute. Cranelift is the default")
            .value_parser(["interp", "cranelift"]))
        .arg(Arg::new("output-format")
            .long("output-format")
            .short('o')
            .value_name("csv|tsv|jsonl|markdown|table|parquet|avro|xlsx")
            .help("If set, records output via print are escaped according to the rules of the corresponding format. jsonl (ndjson) writes stdout as JSON Lines: the first record is the header, a column name may end with :number, :boolean or :auto (default string, or :auto with --infer-types; a boolean is false for false/no/off and for what is false in a condition, such as empty, 0 or 0.0), and each following record becomes a JSON object (a missing column is null; an empty or non-numeric :number value is null, with a warning counting the invalid ones). markdown (md) renders stdout as a Markdown table: the first record is the header row, followed by the delimiter row and the data rows. table renders stdout as an aligned table with box-drawing borders for terminals: the first record is the header, numeric columns are right-aligned, and the table is printed when the program ends. parquet writes stdout as a Parquet file: the first record is the header, a column name may end with :TYPE (BOOLEAN, INT8, INT16, INT32, INT64, FLOAT, DOUBLE, DATE, TIME, TIMESTAMP, JSON, UUID, STRING, or AUTO to infer it from the first rows; default STRING, or AUTO with --infer-types), followed by the data rows. avro writes stdout as an uncompressed Avro Object Container File: the first record is the header, a column name may end with :TYPE (string, int, long, float, double, boolean, date, timestamp, time, uuid, decimal(P,S), bytes as hex, or auto; default string, or auto with --infer-types; an unknown suffix is part of the name), every field is nullable, and names that are not valid Avro names are rewritten with _ (with a warning). xlsx writes stdout as an Excel workbook with one sheet (Sheet1): the first record is the bold, frozen header row, a column name may end with :string, :number, :boolean or :auto (default :auto: a number as JSON writes it and true/false keep their type, while 007 and integers of more than 15 digits stay text), and empty values are blank cells")
            .value_parser(["csv", "tsv", "jsonl", "ndjson", "markdown", "md", "table", "parquet", "avro", "xlsx"]))
        .arg(Arg::new("summary")
            .long("summary")
            .num_args(0)
            .requires("output-format")
            .help("With -o table or -o markdown, print the number of rows and per-column statistics (type, non-empty and distinct counts, min, max, sum and mean) after the table; the first record is the header"))
        .arg(Arg::new("infer-types")
            .long("infer-types")
            .num_args(0)
            .requires("output-format")
            .help("With -o jsonl, -o parquet or -o avro, give columns without a type suffix the type :auto instead of string. In JSON Lines an :auto value is a number when it is written as JSON writes numbers (48, -1.5, 2e3; not 007, +1, 0x10 or 1e999, which is out of range), a boolean for true/false (any case), null when empty and a string otherwise. In Parquet (and Avro) an :auto column is INT64, DOUBLE, BOOLEAN or DATE (YYYY-MM-DD) when every non-empty value of the first 65536 rows is one, and STRING otherwise. In all three formats empty :auto values are null, while empty values of string columns stay empty strings"))
        .arg(Arg::new("null")
            .long("null")
            .value_name("text")
            .help("Read NULLs of database, JSON Lines, Parquet and Avro input as text (default: the empty string), so that they are told apart from empty strings: isnull(s) is 1 for it, -o tsv writes it as \\N (as PostgreSQL COPY and MySQL LOAD DATA read it), -o jsonl, -o parquet and -o avro write it as null, and database query parameters bind it as NULL. For example --null '\\N'"))
        .arg(Arg::new("stats")
            .long("stats")
            .num_args(0)
            .help("When the program exits, print run metrics as a JSON object on stderr for schedulers to collect: input_records (records read from the main input), output_records (records printed to stdout: one per print, one per newline of printf; the header of -o jsonl, parquet, avro, xlsx, markdown or table is not counted), rejected_records (input records skipped because they could not be parsed, such as invalid JSON lines of -i jsonl), elapsed_secs, records_per_sec (input records per second) and exit_code"))
        .arg(Arg::new("program")
            .index(1)
            .help("The zawk program to execute"))
        .arg(Arg::new("input-files")
            .index(2)
            .num_args(1..)
            .help("Input files to be read by zawk program: local paths, `-` (stdin), http(s):// URLs, s3://bucket/key, a nats:// subject, a kafka:// topic, or a database table or query read as CSV with a header line (`sqlite://PATH#table`, `mysql://...#table`, `postgresql://...#select ...`; SQL when the text after `#` contains whitespace)"))
        .arg(Arg::new("parallel-strategy")
            .short('p')
            .help("Attempt to execute the script in parallel. Strategy r[ecord] parallelizes within the current input file. Strategy f[ile] parallelizes between input files. Without -p, an aggregation over local files of 64 MiB or more whose result does not depend on the order of the records (e.g. `{s+=$3} END{print s}`) runs as with -pr; -p0 (or ZAWK_PARALLEL=0) runs it serially")
            .value_parser(["r", "record", "f", "file", "0"]))
        .arg(Arg::new("keep-order")
            .long("keep-order")
            .num_args(0)
            .help("With -pr, write stdout in the order of the input records, as a serial run does (filter and transform scripts can then run in parallel). Implies -pr when -p is not given. Output to files, commands and stderr is not ordered"))
        .arg(Arg::new("chunk-size")
            .long("chunk-size")
            .num_args(1)
            .help("Buffer size when reading input (default: 128 KiB, 512 KiB with `-pr`). This is present primarily for debugging purposes; it's possible that tuning this will help performance, but it should not be necessary"))
        .arg(Arg::new("jobs")
            .short('j')
            .requires("parallel")
            .num_args(1)
            .help("Number or worker threads to launch when executing in parallel, requires '-p' (or '--keep-order') to be set. When using record-level parallelism, this value is an upper bound on the number of worker threads that will be spawned; the number of active worker threads is chosen dynamically"))
        // `-j` takes `-p` or `--keep-order` (which implies `-pr`).
        .group(clap::ArgGroup::new("parallel")
            .args(["parallel-strategy", "keep-order"])
            .multiple(true));
    // display help/version information from awk file
    let known = KnownArgs::of(&app);
    let mut args = command_line_args();
    if args.len() > 2 {
        // sub help from
        let last_pair = args.last().unwrap();
        if last_pair == "--help"
            || last_pair == "-h"
            || last_pair == "--version"
            || last_pair == "-v"
        {
            let exec_supplied = args
                .iter()
                .any(|item| item == "-E" || item.starts_with("--exec"));
            let file_supplied = args
                .iter()
                .any(|item| item == "-f" || item.starts_with("--program-file"));
            // The arguments after `-E file` are the script's own, but with `-f` a trailing `-v` is the
            // assignment option missing its value, and a flag after an option taking a value is that value.
            let sub_flag = exec_supplied
                || (file_supplied && last_pair != "-v" && !takes_next_value(&args[args.len() - 2]));
            if sub_flag {
                let last_pair = last_pair.clone();
                args.remove(args.len() - 1); // remove --help and --version
                let matches = get_matches(app, end_options(args, &known));
                let awk_file = matches
                    .get_one::<String>("exec")
                    .or_else(|| matches.get_one::<String>("program-file"))
                    .unwrap();
                // Remote programs (URL, `gh:`) are fetched like for a run; a read error is fatal.
                let (awk_code, _) = read_program_file(awk_file);
                if last_pair.contains("-h") {
                    awk_util::print_awk_file_help(awk_file, &awk_code);
                } else {
                    awk_util::print_awk_file_version(&awk_code);
                }
                return;
            }
        }
    }
    let matches = get_matches(app, end_options(args, &known));
    // dump sub command
    if let Some(matches) = matches.subcommand_matches("dump") {
        let input_file = matches.get_one::<String>("input-file").unwrap();
        if matches.get_flag("prometheus") {
            let text = runtime::csv::parse_prometheus(input_file);
            println!("{}", text);
            return;
        }
        fail!("dump: no format given; only --prometheus is supported");
    }
    // functions sub command
    if let Some(matches) = matches.subcommand_matches("functions") {
        let docs = function_docs::find(matches.get_one::<String>("filter").map(|s| s.as_str()));
        if matches.get_flag("json") {
            print!("{}", function_docs::render_ndjson(&docs));
        } else {
            print!("{}", function_docs::render_text(&docs));
        }
        return;
    }
    // lint sub command
    if let Some(matches) = matches.subcommand_matches("lint") {
        run_lint(matches);
    }
    // fmt sub command
    if let Some(matches) = matches.subcommand_matches("fmt") {
        run_fmt(matches);
    }
    // test sub command
    if let Some(matches) = matches.subcommand_matches("test") {
        let paths: Vec<&String> = matches
            .get_many::<String>("paths")
            .map_or_else(Vec::new, |paths| paths.collect());
        let filter = matches.get_one::<String>("filter").map(String::as_str);
        match test_runner::run(&paths, filter) {
            Ok(true) => return,
            Ok(false) => std::process::exit(1),
            Err(e) => fatal!("{}", e),
        }
    }
    // inspect sub command
    if let Some(matches) = matches.subcommand_matches("inspect") {
        let opts = inspect::Options {
            format: matches
                .get_one::<String>("input-format")
                .and_then(|f| inspect::Format::parse(f)),
            style: match matches
                .get_one::<String>("output-format")
                .map(|s| s.as_str())
            {
                Some("markdown") | Some("md") => inspect::Style::Markdown,
                _ => inspect::Style::Table,
            },
            sample_rows: *matches.get_one::<usize>("rows").unwrap(),
            max_rows: matches.get_one::<u64>("max-rows").copied(),
            header: true,
        };
        for (i, file) in matches.get_many::<String>("files").unwrap().enumerate() {
            match inspect::inspect(file, &opts) {
                Ok(report) => {
                    if i > 0 {
                        println!();
                    }
                    print!("{}", report);
                }
                Err(e) => {
                    eprintln!("zawk: {}", e);
                    std::process::exit(2);
                }
            }
        }
        return;
    }
    // llm sub command
    if let Some(matches) = matches.subcommand_matches("llm") {
        run_llm(matches);
    }
    // init sub command
    if let Some(matches) = matches.subcommand_matches("init") {
        let mut awk_file = matches.get_one::<String>("awk-file").unwrap().clone();
        if !awk_file.ends_with(".awk") {
            awk_file = format!("{}.awk", awk_file);
        }
        let author = whoami::username().unwrap_or("".to_owned());
        let template = include_str!("templates/demo.awk");
        let template = template.replace("$USER", &author);
        let mut tasksh_file = File::create(&awk_file).unwrap();
        tasksh_file.write_all(template.as_bytes()).unwrap();
        set_executable(&awk_file);
        println!("{} created", awk_file);
        return;
    }
    let ifmt = match matches
        .get_one::<String>("input-format")
        .map(|s| s.as_str())
    {
        Some("csv") => Some(InputFormat::CSV),
        Some("tsv") => Some(InputFormat::TSV),
        Some("json") | Some("jsonl") | Some("ndjson") | Some("parquet") | Some("avro")
        | Some("markdown") | Some("md") | Some("xlsx") => None,
        Some(x) => fail!("invalid input format: {}", x),
        None => None,
    };
    let mut exec_strategy = match matches
        .get_one::<String>("parallel-strategy")
        .map(|s| s.as_str())
    {
        Some("r") | Some("record") => ExecutionStrategy::ShardPerRecord(num_cpus::get()),
        Some("f") | Some("file") => ExecutionStrategy::ShardPerFile,
        // `--keep-order` alone runs as `-pr --keep-order`.
        None if matches.get_flag("keep-order") => {
            ExecutionStrategy::ShardPerRecord(num_cpus::get())
        }
        Some("0") | None => ExecutionStrategy::Serial,
        Some(x) => fail!(
            "invalid execution strategy (clap arg parsing should handle this): {}",
            x
        ),
    };
    if matches.get_flag("keep-order") && matches!(exec_strategy, ExecutionStrategy::ShardPerFile) {
        fail!("--keep-order only works with -pr, not with -pf");
    }

    // NB: do we want this to be a command-line param?
    // The default depends on the execution strategy, which may still change below.
    let chunk_size_flag =
        matches
            .get_one::<String>("chunk-size")
            .map(|cs| match cs.parse::<usize>() {
                Ok(0) => fail!("value of 'chunk-size' flag must be at least 1"),
                Ok(u) => u,
                Err(e) => fail!("value of 'chunk-size' flag must be numeric: {}", e),
            });
    let mut num_workers = match matches.get_one::<String>("jobs") {
        Some(s) => match s.parse::<usize>() {
            Ok(u) => u,
            Err(e) => fail!("value of 'jobs' flag must be numeric: {}", e),
        },
        None => exec_strategy.num_workers(),
    };
    if let ExecutionStrategy::ShardPerRecord(n) = &mut exec_strategy {
        // The reader thread reads ahead a number of chunks proportional to `-j`, not to the
        // number of CPUs.
        *n = num_workers.max(1);
    }
    // As in gawk, ARGV[0] is the name zawk was invoked with, without its directory.
    let program_name = std::env::args_os()
        .next()
        .and_then(|arg0| {
            std::path::Path::new(&arg0)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| String::from("zawk"));
    let mut input_files: Vec<String> = matches
        .get_many::<String>("input-files")
        .map(|x| x.map(String::from).collect())
        .unwrap_or_default();
    // get awk program code
    // `-f`, `-E`, `-e` and `--include` sources, in command-line order.
    let mut sources: Vec<(usize, SourceKind, &String)> = Vec::new();
    for (id, kind) in [
        ("program-file", SourceKind::File),
        ("exec", SourceKind::File),
        ("source", SourceKind::Inline),
        ("include", SourceKind::Include),
    ] {
        if let (Some(idx), Some(vals)) = (matches.indices_of(id), matches.get_many::<String>(id)) {
            sources.extend(idx.zip(vals).map(|(i, v)| (i, kind, v)));
        }
    }
    sources.sort_by_key(|(i, _, _)| *i);
    // The program sources spliced with the files they include, in pieces that remember where in
    // their file they start; also the sources for `--lint`.
    let mut includer = include::Includer::default();
    // `--include` files are libraries: alone, they do not replace the program operand.
    let program_sources = sources
        .iter()
        .filter(|(_, kind, _)| *kind != SourceKind::Include)
        .count();
    if program_sources > 0 {
        // We specified the program with options, so the "program" will be
        // interpreted as another input file.
        if let Some(p) = matches.get_one::<String>("program") {
            input_files.insert(0, p.into());
        }
        // A lone `-e` is reported as `line 1, column 7`; with other sources, each `-e` is
        // named `-e#N` so that its lines are not taken for lines of another source.
        let named = program_sources > 1;
        let mut inline_count = 0;
        for (_, kind, prog_file) in sources {
            let res = match kind {
                SourceKind::Include => includer.include(prog_file),
                SourceKind::Inline => {
                    inline_count += 1;
                    let name = named.then(|| parsing::inline_source_name(inline_count));
                    let text = inline_program(prog_file);
                    includer.add_source(name, text.as_deref().unwrap_or(prog_file), None)
                }
                SourceKind::File => {
                    let (text, path) = read_program_file(prog_file);
                    includer.add_source(Some(prog_file.clone()), &text, path)
                }
            };
            if let Err(e) = res {
                include_failed(&e);
            }
        }
    } else if let Some(p) = matches.get_one::<String>("program") {
        if program_operand_file(p) {
            let _ = PROGRAM_OPERAND_FILE.set(p.clone());
        }
        for (_, _, file) in sources {
            if let Err(e) = includer.include(file) {
                include_failed(&e);
            }
        }
        // Sugar syntax only applies to programs given on the command line, not to `-f` files.
        // A Markdown program (e.g. the answer of an LLM) is its first awk code block, whose lines
        // keep their numbers in the Markdown text.
        let p = match inline_program(p) {
            Some(code) if matches.get_flag("posix") => code,
            Some(code) => awk_util::sugar_syntax_convert(code),
            None if matches.get_flag("posix") => p.clone(),
            None => awk_util::sugar_syntax_convert(String::from(p.trim())),
        };
        if let Err(e) = includer.add_source(None, &p, None) {
            include_failed(&e);
        }
    } else {
        fail!("must specify program at command line, with -e, or in a file via -f");
    }
    // Where each piece starts in the program, to report errors as `file:line:column`.
    let mut source_map = parsing::SourceMap::default();
    let mut program_code = String::new();
    for piece in &includer.pieces {
        source_map.push(
            piece.name.clone(),
            program_code.matches('\n').count(),
            piece.line,
        );
        program_code.push_str(&piece.text);
        program_code.push('\n');
    }
    if matches.get_flag("lint") || matches.get_flag("deny-warnings") {
        lint_before_run(
            &includer.pieces,
            &matches,
            &input_files,
            input_format(&matches, &input_files).as_deref(),
        );
    }
    parsing::set_program(&program_code, source_map);
    // Build ARGV from the final input files: with `-f`, the first operand is parsed by clap
    // as `program` and only gets added to `input_files` above.
    let argv: Vec<String> = std::iter::once(program_name)
        .chain(input_files.iter().cloned())
        .collect();
    // `var=value` operands stay in ARGV, but are not read as input files.
    let (input_files, operand_assigns) = split_operands(&input_files);
    if !operand_assigns.is_empty() && !matches!(exec_strategy, ExecutionStrategy::Serial) {
        // The assignments run as the main loop reaches each file, which workers cannot follow.
        exec_strategy = ExecutionStrategy::Serial;
        num_workers = 1;
    }
    let (escaper, output_sep, output_record_sep) = match matches
        .get_one::<String>("output-format")
        .map(|s| s.as_str())
    {
        Some("csv") => (Escaper::CSV, Some(","), Some("\n")),
        Some("tsv") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and converted to JSON objects by `JsonlFactory`.
        Some("jsonl") | Some("ndjson") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and rendered as table rows by `MarkdownFactory`.
        Some("markdown") | Some("md") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and rendered as a bordered table by `TableFactory`.
        Some("table") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and converted to a Parquet file by `ParquetFactory`.
        Some("parquet") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and converted to an Avro file by `AvroFactory`.
        Some("avro") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and converted to an Excel workbook by `XlsxFactory`.
        Some("xlsx") => (Escaper::TSV, Some("\t"), Some("\n")),
        Some(s) => fail!(
            "invalid output format {:?}; expected csv, tsv, jsonl, markdown, table, parquet, avro or xlsx (or the empty string)",
            s
        ),
        None => (Escaper::Identity, None, None),
    };
    runtime::str_impl::set_characters_as_bytes(matches.get_flag("characters-as-bytes"));
    if let Some(text) = matches.get_one::<String>("null") {
        runtime::str_escape::set_null_text(text.clone());
    }
    let input_format = input_format(&matches, &input_files);
    warn_binary_read_as_text(input_format.as_deref(), &input_files);
    let json = matches!(input_format.as_deref(), Some("json"));
    let jsonl = matches!(input_format.as_deref(), Some("jsonl") | Some("ndjson"));
    let parquet = matches!(input_format.as_deref(), Some("parquet"));
    let avro = matches!(input_format.as_deref(), Some("avro"));
    let markdown = matches!(input_format.as_deref(), Some("markdown") | Some("md"));
    // Database tables and queries are read as CSV, the column names on the header line.
    let database = matches!(input_format.as_deref(), Some("database"));
    // A sheet of an Excel workbook is read as CSV; with -H, its first row is the header.
    let xlsx = matches!(input_format.as_deref(), Some("xlsx"));
    if input_files.iter().any(|f| database::is_db_url(f))
        && !(database || ifmt == Some(InputFormat::CSV))
    {
        fail!(
            "database input is read as CSV: -i {} is not supported",
            input_format.as_deref().unwrap_or_default()
        );
    }
    // Workbooks mixed with other files are read as CSV (see `input_format`).
    let ifmt = if database || (ifmt.is_none() && input_format.as_deref() == Some("csv")) {
        Some(InputFormat::CSV)
    } else {
        ifmt
    };
    // JSON documents, JSON Lines records, Parquet and Avro files, Markdown tables and database
    // tables are self-describing, so FI is always populated from the first record (the column
    // names for Parquet, Avro, Markdown and database tables).
    let parse_header = matches.get_flag("parse-header")
        || json
        || jsonl
        || parquet
        || avro
        || markdown
        || database;
    // A NATS subject or a Kafka topic: each message is an input file of its own, read until Ctrl+C.
    let message_url = input_files
        .iter()
        .find(|f| nats::is_nats_url(f) || kafka::is_kafka_url(f))
        .cloned();
    if let Some(url) = &message_url {
        if input_files.len() > 1 {
            fail!(
                "a {} must be the only input",
                if kafka::is_kafka_url(url) {
                    "kafka:// topic"
                } else {
                    "nats:// subject"
                }
            );
        }
        if parquet || avro || markdown || xlsx {
            let service = if kafka::is_kafka_url(url) {
                "Kafka"
            } else {
                "NATS"
            };
            fail!(
                "-i {} is not supported for {} messages",
                input_format.as_deref().unwrap_or_default(),
                service
            );
        }
        exec_strategy = ExecutionStrategy::Serial;
        num_workers = 1;
    }
    parsing::set_workers_read_whole_files(matches!(exec_strategy, ExecutionStrategy::ShardPerFile));
    // Every input file starts with a header line (JSON, Parquet and Avro records are mapped by
    // key).
    // Every message is read by a reader of its own, which starts with the header (the keys
    // of a JSON message).
    let header_per_file =
        parse_header && ((!json && !jsonl && !parquet && !avro) || message_url.is_some());
    let multi_file_headers = header_per_file && (input_files.len() > 1 || message_url.is_some());
    if multi_file_headers && !matches!(exec_strategy, ExecutionStrategy::Serial) {
        // The workers cannot tell which record is the header of a file.
        eprintln!("zawk: warning: -H with several input files: running serially");
        exec_strategy = ExecutionStrategy::Serial;
        num_workers = 1;
    }
    // Without `-p`, a program whose result does not depend on how its records are shared out
    // (an aggregation such as `{s+=$3} END{print s}`) runs with `-pr` on large local files.
    if matches.get_one::<String>("parallel-strategy").is_none()
        && matches!(exec_strategy, ExecutionStrategy::Serial)
        && message_url.is_none()
        && operand_assigns.is_empty()
        && !multi_file_headers
        && auto_parallel_enabled()
        && auto_parallel_inputs(&input_files, input_format.as_deref())
    {
        let given: Vec<&str> = matches
            .get_many::<String>("var")
            .into_iter()
            .flatten()
            .map(|v| v.split_once('=').map_or(v.as_str(), |(n, _)| n).trim())
            .collect();
        if !given.contains(&"RS") && auto_parallel::record_parallel_safe(&program_code, &given) {
            num_workers = num_cpus::get();
            exec_strategy = ExecutionStrategy::ShardPerRecord(num_workers);
        }
    }
    // `--keep-order` promises the output of a serial run, which a range pattern only gets when
    // one thread sees every record (each worker keeps its own range state).
    if matches.get_flag("keep-order")
        && matches!(exec_strategy, ExecutionStrategy::ShardPerRecord(_))
        && lint::has_range_pattern(&includer.pieces)
    {
        eprintln!(
            "zawk: warning: --keep-order: a range pattern (`start, end`) needs every record in one thread: running serially"
        );
        exec_strategy = ExecutionStrategy::Serial;
        num_workers = 1;
    }
    // With `-pr --keep-order`, the workers write stdout in the order of the input (a serial run
    // writes it in that order anyway).
    runtime::writers::ordered::set_keep_order(
        matches.get_flag("keep-order")
            && matches!(exec_strategy, ExecutionStrategy::ShardPerRecord(_)),
    );
    // With `-pr`, larger chunks mean fewer round trips between the reader and the workers (25-35%
    // faster on GB-sized inputs; 1 MiB is no faster and leaves workers idle at the end of a few
    // hundred MB); serial runs keep smaller chunks, which stay in L2 and add less latency on pipes.
    let chunk_size = chunk_size_flag.unwrap_or(match exec_strategy {
        ExecutionStrategy::ShardPerRecord(_) => PARALLEL_CHUNK_SIZE,
        _ => CHUNK_SIZE,
    });
    // Variables that END sees summed over the workers (`max`, `last`, a flag) give a wrong
    // result silently: warn about them, unless `--lint` already reported them.
    let linted = matches.get_flag("lint") || matches.get_flag("deny-warnings");
    if !linted {
        let mode = match exec_strategy {
            ExecutionStrategy::Serial => None,
            ExecutionStrategy::ShardPerRecord(_) => Some(lint::Parallel::Record),
            ExecutionStrategy::ShardPerFile => Some(lint::Parallel::File),
        };
        if let Some(mode) = mode {
            let keep_order = runtime::writers::ordered::keep_order();
            for w in lint::parallel_warnings(&includer.pieces, mode, keep_order) {
                eprintln!("{}", w);
            }
        }
    }

    let opt_level: i32 = match matches.get_one::<String>("opt-level").map(|s| s.as_str()) {
        Some("3") => 3,
        Some("2") => 2,
        Some("1") => 1,
        Some("0") => 0,
        Some("-1") => -1,
        None => DEFAULT_OPT_LEVEL,
        Some(x) => panic!(
            "this case should be covered by clap argument validation: found unexpected opt-level value {}",
            x
        ),
    };
    let raw = RawPrelude {
        field_sep: matches
            .get_one::<String>("field-separator")
            .map(String::from),
        var_decs: matches
            .get_many::<String>("var")
            .map(|x| x.map(String::from).collect())
            .unwrap_or_default(),
        output_sep,
        scalars: PreludeScalars {
            escaper,
            fold_regexes: opt_level >= 3,
            stage: exec_strategy.stage(),
            parse_header,
            header_per_file,
            header_in_main: message_url.is_some(),
            test_entry: matches.get_one::<String>("test-entry").cloned(),
        },
        output_record_sep,
        argv,
        operand_files: if operand_assigns.is_empty() {
            Vec::new()
        } else {
            input_files.clone()
        },
        operand_assigns,
    };
    let opt_dump_bytecode = matches.get_flag("dump-bytecode");
    let opt_dump_cfg = matches.get_flag("dump-cfg");
    let skip_output = opt_dump_bytecode || opt_dump_cfg;
    if opt_dump_bytecode {
        let _ = write!(
            io::stdout(),
            "{}",
            dump_bytecode(program_code.as_str(), &raw),
        );
    }
    if opt_dump_cfg {
        let a = Arena::default();
        let ctx = get_context(program_code.as_str(), &a, get_prelude(&a, &raw));
        let mut stdout = io::stdout();
        let _ = ctx.dbg_print(&mut stdout);
    }
    if skip_output {
        return;
    }
    // Report unreadable S3 objects (missing settings, bucket or object, denied access) before
    // any input is processed.
    for file in input_files.iter().filter(|f| runtime::s3::is_s3_url(f)) {
        if let Err(e) = runtime::s3::check_object(file) {
            fatal!("{}", e);
        }
    }
    let mut message_input = message_url.map(|url| {
        let source = if kafka::is_kafka_url(&url) {
            kafka::Subscription::new(&url).map(|s| Box::new(s) as Box<dyn MessageSource>)
        } else {
            nats::Subscription::new(&url).map(|s| Box::new(s) as Box<dyn MessageSource>)
        };
        match source {
            Ok(source) => {
                // the output of each message is written as soon as it is printed
                runtime::writers::set_line_buffered();
                source
            }
            Err(e) => fatal!("{}", e),
        }
    });
    runtime::set_multi_file_headers(multi_file_headers);
    // Text, CSV and TSV files: FILENAME and FNR in END when the last of them is empty.
    if message_input.is_none()
        && !database
        && matches!(input_format.as_deref(), None | Some("csv") | Some("tsv"))
        && let Some(last) = input_files.last()
    {
        runtime::set_last_input_file(last, input_files.len());
    }
    let check_utf8 = matches.get_flag("utf8");
    let signal = CancelSignal::default();

    // This horrid macro is here because all the different ways of reading input are different
    // types, making functions hard to write. Still, there must be something to be done to clean
    // this up here.
    macro_rules! with_inp {
        ($analysis:expr, $inp:ident, $body:expr) => {{
            if let Some(source) = message_input.take() {
                // Each message is read with a reader of the input format of its own.
                if json || jsonl {
                    let fi_names = awk_util::fi_constant_keys(&program_code);
                    let signal = signal.clone();
                    let $inp =
                        MessageReader::new(source, json, check_utf8, move |payload, subject| {
                            let chunk_size = message::chunk_size(chunk_size, payload.len());
                            let reader = JsonlReader::new(
                                once((message::payload_reader(payload), subject)),
                                fi_names.clone(),
                                chunk_size,
                                false,
                                ExecutionStrategy::Serial,
                                signal.clone(),
                            );
                            if json { reader.scalar_text() } else { reader }
                        });
                    $body
                } else if let Some(ifmt) = ifmt {
                    let signal = signal.clone();
                    let $inp =
                        MessageReader::new(source, false, check_utf8, move |payload, subject| {
                            let chunk_size = message::chunk_size(chunk_size, payload.len());
                            CSVReader::new(
                                once((message::payload_reader(payload), subject)),
                                ifmt,
                                chunk_size,
                                false,
                                ExecutionStrategy::Serial,
                                signal.clone(),
                            )
                        });
                    $body
                } else {
                    let $inp =
                        MessageReader::new(source, false, check_utf8, move |payload, subject| {
                            let chunk_size = message::chunk_size(chunk_size, payload.len());
                            RegexSplitter::new(
                                message::payload_reader(payload),
                                chunk_size,
                                subject,
                                false,
                            )
                        });
                    $body
                }
            } else if markdown {
                // The first table of a Markdown file is read as CSV with a header line.
                let sources: Vec<(MarkdownCsv, String)> = if input_files.len() == 0 {
                    vec![(MarkdownCsv::stdin(), String::from("-"))]
                } else {
                    input_files
                        .iter()
                        .map(|file| match file.as_str() {
                            "-" => (MarkdownCsv::stdin(), file.clone()),
                            _ => (MarkdownCsv::file(file.clone()), file.clone()),
                        })
                        .collect()
                };
                let $inp = CSVReader::new(
                    sources.into_iter(),
                    InputFormat::CSV,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                )
                .plain_getline();
                $body
            } else if xlsx {
                // A sheet of an Excel workbook is read as CSV.
                let sources: Vec<(xlsx::XlsxCsv, String)> = if input_files.len() == 0 {
                    vec![(xlsx::XlsxCsv::new(String::from("-")), String::from("-"))]
                } else {
                    input_files
                        .iter()
                        .map(|file| (xlsx::XlsxCsv::new(file.clone()), file.clone()))
                        .collect()
                };
                let $inp = CSVReader::new(
                    sources.into_iter(),
                    InputFormat::CSV,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                )
                .plain_getline();
                $body
            } else if parquet {
                // Parquet rows are read as JSON Lines.
                let fi_names = awk_util::fi_constant_keys(&program_code);
                let projection = parquet_projection(program_code.as_str(), &raw, &fi_names);
                let open = |source: ParquetJson| match &projection {
                    Some(p) => source.with_projection(p.clone()),
                    None => source,
                };
                let sources: Vec<(ParquetJson, String)> = if input_files.len() == 0 {
                    vec![(open(ParquetJson::stdin()), String::from("-"))]
                } else {
                    for file in input_files.iter().filter(|f| f.as_str() != "-") {
                        if let Err(e) = ParquetJson::check(file) {
                            fatal!("{}", e);
                        }
                    }
                    input_files
                        .iter()
                        .map(|file| match file.as_str() {
                            "-" => (open(ParquetJson::stdin()), file.clone()),
                            _ => (open(ParquetJson::file(file.clone())), file.clone()),
                        })
                        .collect()
                };
                let $inp = JsonlReader::new(
                    sources.into_iter(),
                    fi_names,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                );
                $body
            } else if avro {
                // Avro records are read as JSON Lines.
                let fi_names = awk_util::fi_constant_keys(&program_code);
                let sources: Vec<(AvroJson, String)> = if input_files.len() == 0 {
                    vec![(AvroJson::stdin(), String::from("-"))]
                } else {
                    for file in input_files.iter().filter(|f| f.as_str() != "-") {
                        if let Err(e) = AvroJson::check(file) {
                            fatal!("{}", e);
                        }
                    }
                    input_files
                        .iter()
                        .map(|file| match file.as_str() {
                            "-" => (AvroJson::stdin(), file.clone()),
                            _ => (AvroJson::file(file.clone()), file.clone()),
                        })
                        .collect()
                };
                let $inp = JsonlReader::new(
                    sources.into_iter(),
                    fi_names,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                );
                $body
            } else if json {
                // JSON documents are read as JSON Lines, a JSONPath (`file.json#$.data`) selecting
                // the records.
                let fi_names = awk_util::fi_constant_keys(&program_code);
                let operands: Vec<String> = if input_files.len() == 0 {
                    vec![String::from("-")]
                } else {
                    input_files.clone()
                };
                let sources: Vec<_> = operands
                    .into_iter()
                    .map(|operand| {
                        let file = runtime::splitter::json::split_path(&operand).0.to_string();
                        (
                            JsonRecords::new(open_file_read(file.as_str()), &operand),
                            file,
                        )
                    })
                    .collect();
                let $inp = JsonlReader::new(
                    sources.into_iter(),
                    fi_names,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                )
                .scalar_text();
                $body
            } else if jsonl {
                let fi_names = awk_util::fi_constant_keys(&program_code);
                if input_files.len() == 0 {
                    let _reader: Box<dyn io::Read + Send> = Box::new(io::stdin());
                    let $inp = JsonlReader::new(
                        once((_reader, String::from("-"))),
                        fi_names,
                        chunk_size,
                        check_utf8,
                        exec_strategy,
                        signal.clone(),
                    );
                    $body
                } else {
                    let file_handles: Vec<_> = input_files
                        .iter()
                        .cloned()
                        .map(|file| (open_file_read(file.as_str()), file))
                        .collect();
                    let $inp = JsonlReader::new(
                        file_handles.into_iter(),
                        fi_names,
                        chunk_size,
                        check_utf8,
                        exec_strategy,
                        signal.clone(),
                    );
                    $body
                }
            } else if input_files.len() == 0 {
                let _reader: Box<dyn io::Read + Send> = Box::new(io::stdin());
                match (ifmt, $analysis) {
                    (Some(ifmt), _) => {
                        let $inp = CSVReader::new(
                            once((_reader, String::from("-"))),
                            ifmt,
                            chunk_size,
                            check_utf8,
                            exec_strategy,
                            signal.clone(),
                        );
                        $body
                    }
                    (
                        None,
                        cfg::SepAssign::Potential {
                            field_sep,
                            record_sep,
                        },
                    ) => {
                        let field_sep = field_sep.unwrap_or(b" ");
                        let record_sep = record_sep.unwrap_or(b"\n");
                        // The default FS splits on runs of blanks and newlines, which the
                        // single-byte splitter only handles with the default RS.
                        if field_sep.len() == 1
                            && record_sep.len() == 1
                            && (field_sep != b" " || record_sep == b"\n")
                        {
                            if field_sep == b" " && record_sep == b"\n" {
                                let $inp = ByteReader::new_whitespace(
                                    once((_reader, String::from("-"))),
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            } else {
                                let $inp = ByteReader::new(
                                    once((io::stdin(), String::from("-"))),
                                    field_sep[0],
                                    record_sep[0],
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            }
                        } else if record_sep.len() == 1 {
                            // Other field separators are split from `$0` in the workers.
                            let $inp = ByteReader::new_records(
                                once((_reader, String::from("-"))),
                                record_sep[0],
                                chunk_size,
                                check_utf8,
                                exec_strategy,
                                signal.clone(),
                            );
                            $body
                        } else {
                            warn_regex_splitter(exec_strategy, Some(record_sep));
                            let $inp =
                                chained(RegexSplitter::new(_reader, chunk_size, "-", check_utf8));
                            $body
                        }
                    }
                    (None, cfg::SepAssign::Unsure) => {
                        warn_regex_splitter(exec_strategy, None);
                        let $inp =
                            chained(RegexSplitter::new(_reader, chunk_size, "-", check_utf8));
                        $body
                    }
                }
            } else if let Some(ifmt) = ifmt {
                // A workbook is converted to CSV (or TSV) rather than read as raw bytes.
                let file_handles: Vec<_> = input_files
                    .iter()
                    .cloned()
                    .map(|file| {
                        let reader: Box<dyn io::Read + Send> = if xlsx::is_xlsx_file(&file) {
                            Box::new(match ifmt {
                                InputFormat::TSV => xlsx::XlsxCsv::tsv(file.clone()),
                                _ => xlsx::XlsxCsv::new(file.clone()),
                            })
                        } else {
                            Box::new(open_file_read(file.as_str()))
                        };
                        (reader, file)
                    })
                    .collect();
                let $inp = CSVReader::new_with_pread(
                    file_handles.into_iter(),
                    pread_inputs(&input_files, exec_strategy, check_utf8),
                    ifmt,
                    chunk_size,
                    check_utf8,
                    exec_strategy,
                    signal.clone(),
                );
                $body
            } else {
                match $analysis {
                    cfg::SepAssign::Potential {
                        field_sep,
                        record_sep,
                    } => {
                        let field_sep = field_sep.unwrap_or(b" ");
                        let record_sep = record_sep.unwrap_or(b"\n");
                        // The default FS splits on runs of blanks and newlines, which the
                        // single-byte splitter only handles with the default RS.
                        if field_sep.len() == 1
                            && record_sep.len() == 1
                            && (field_sep != b" " || record_sep == b"\n")
                        {
                            let file_handles: Vec<_> = input_files
                                .iter()
                                .cloned()
                                .map(move |file| (open_file_read(file.as_str()), file))
                                .collect();
                            let pread = pread_inputs(&input_files, exec_strategy, check_utf8);
                            if field_sep == b" " && record_sep == b"\n" {
                                let $inp = ByteReader::new_whitespace_with_pread(
                                    file_handles.into_iter(),
                                    pread,
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            } else {
                                let $inp = ByteReader::new_with_pread(
                                    file_handles.into_iter(),
                                    pread,
                                    field_sep[0],
                                    record_sep[0],
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            }
                        } else if record_sep.len() == 1 {
                            // Other field separators are split from `$0` in the workers.
                            let file_handles: Vec<_> = input_files
                                .iter()
                                .cloned()
                                .map(move |file| (open_file_read(file.as_str()), file))
                                .collect();
                            let $inp = ByteReader::new_records_with_pread(
                                file_handles.into_iter(),
                                pread_inputs(&input_files, exec_strategy, check_utf8),
                                record_sep[0],
                                chunk_size,
                                check_utf8,
                                exec_strategy,
                                signal.clone(),
                            );
                            $body
                        } else {
                            warn_regex_splitter(exec_strategy, Some(record_sep));
                            let iter = input_files.iter().cloned().map(|file| {
                                let reader: Box<dyn io::Read + Send> =
                                    Box::new(open_file_read(file.as_str()));
                                RegexSplitter::new(reader, chunk_size, file, check_utf8)
                            });
                            let $inp = ChainedReader::new(iter);
                            $body
                        }
                    }
                    cfg::SepAssign::Unsure => {
                        warn_regex_splitter(exec_strategy, None);
                        let iter = input_files.iter().cloned().map(|file| {
                            let reader: Box<dyn io::Read + Send> =
                                Box::new(open_file_read(file.as_str()));
                            RegexSplitter::new(reader, chunk_size, file, check_utf8)
                        });
                        let $inp = ChainedReader::new(iter);
                        $body
                    }
                }
            }
        }};
    }

    // validate AWK code by comment tags
    let passed =
        awk_util::validate_awk_code(&program_code, &raw.var_decs, &raw.operand_assigns.concat());
    if !passed {
        std::process::exit(1);
    }
    let a = Arena::default();
    let ctx = get_context(program_code.as_str(), &a, get_prelude(&a, &raw));
    let analysis_result = ctx.analyze_sep_assignments();
    let out_file = matches.get_one::<String>("out-file");
    // stdout wrappers for output formats that are not escaped text.
    let output_format = match matches
        .get_one::<String>("output-format")
        .map(|s| s.as_str())
    {
        Some("jsonl") | Some("ndjson") => "jsonl",
        Some("markdown") | Some("md") => "markdown",
        Some("table") => "table",
        Some("parquet") => "parquet",
        Some("avro") => "avro",
        Some("xlsx") => "xlsx",
        _ => "",
    };
    let summary = matches.get_flag("summary");
    if summary && !matches!(output_format, "markdown" | "table") {
        fail!("--summary requires -o table or -o markdown");
    }
    let infer_types = matches.get_flag("infer-types");
    if infer_types && !matches!(output_format, "jsonl" | "parquet" | "avro") {
        fail!("--infer-types requires -o jsonl, -o parquet or -o avro");
    }
    if matches.get_flag("stats") {
        runtime::stats::enable(start, !output_format.is_empty());
    }
    if matches!(output_format, "parquet" | "avro" | "xlsx")
        && out_file.is_none()
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        fatal!(
            "refusing to write {} output to a terminal; use --out-file FILE or redirect stdout",
            output_format
        );
    }
    macro_rules! with_out {
        ($factory:expr, |$inp:ident, $out:ident| $body:expr) => {
            match output_format {
                "jsonl" => {
                    let $out = runtime::writers::JsonlFactory($factory, infer_types);
                    with_inp!(analysis_result, $inp, $body);
                }
                "markdown" => {
                    let $out = runtime::writers::MarkdownFactory($factory, summary);
                    with_inp!(analysis_result, $inp, $body);
                }
                "table" => {
                    let $out = runtime::writers::TableFactory($factory, summary);
                    with_inp!(analysis_result, $inp, $body);
                }
                "parquet" => {
                    let $out = runtime::parquet_writer::ParquetFactory($factory, infer_types);
                    with_inp!(analysis_result, $inp, $body);
                }
                "avro" => {
                    let $out = runtime::avro_writer::AvroFactory($factory, infer_types);
                    with_inp!(analysis_result, $inp, $body);
                }
                "xlsx" => {
                    let $out = runtime::xlsx_writer::XlsxFactory($factory);
                    with_inp!(analysis_result, $inp, $body);
                }
                _ => {
                    let $out = $factory;
                    with_inp!(analysis_result, $inp, $body);
                }
            }
        };
    }
    macro_rules! with_io {
        (|$inp:ident, $out:ident| $body:expr) => {
            match out_file {
                Some(oup) => with_out!(
                    runtime::writers::factory_from_file(oup).unwrap_or_else(|e| fatal!(
                        "failed to open {}: {}",
                        oup,
                        e
                    )),
                    |$inp, $out| $body
                ),
                None => with_out!(runtime::writers::default_factory(), |$inp, $out| $body),
            }
        };
    }
    // `-O -1` forces the bytecode interpreter.
    let backend = match (
        matches.get_one::<String>("backend").map(|s| s.as_str()),
        opt_level,
    ) {
        (Some("cranelift"), -1) => {
            fail!("`-O -1` forces the bytecode interpreter and conflicts with `-B cranelift`")
        }
        (None, -1) => Some("interp"),
        (b, _) => b,
    };
    match backend {
        Some("interp") => {
            with_io!(|inp, oup| run_interp_with_context(ctx, inp, oup, num_workers, signal))
        }
        None | Some("cranelift") => {
            with_io!(|inp, oup| run_cranelift_with_context(
                ctx,
                inp,
                oup,
                codegen::Config {
                    opt_level: opt_level.max(0) as usize,
                    num_workers,
                },
                signal,
            ));
        }
        Some(b) => {
            fail!("invalid backend: {:?}", b);
        }
    }
    // Output that could not be written (reported when files were flushed at exit) is an error.
    let rc = runtime::writers::exit_code(0);
    if rc != 0 {
        std::process::exit(rc);
    }
}

#[cfg(unix)]
fn set_executable(path: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn set_executable(path: &str) {}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn non_utf8_args_are_escaped() {
        let arg = std::ffi::OsStr::from_bytes(b"in\xff\xa7\xe4\xb8\xad.txt");
        assert_eq!(escape_non_utf8(arg), "in\\xFF\\xA7中.txt");
        // The escapes decode back to the bytes, e.g. for `-F $'\xa7'`.
        let a = Arena::default();
        let mut buf = Vec::new();
        assert_eq!(
            lexer::parse_string_literal(
                &escape_non_utf8(std::ffi::OsStr::from_bytes(b"\xa7")),
                &a,
                &mut buf
            ),
            b"\xa7"
        );
    }
}
