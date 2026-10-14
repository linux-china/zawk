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
pub mod compile;
pub mod cross_stage;
pub mod dataflow;
mod display;
pub mod dom;
mod function_docs;
#[cfg(test)]
pub mod harness;
mod input_taint;
mod strnum_analysis;
pub mod interp;
pub mod lexer;
mod lint;
#[allow(unused_parens)] // Warnings appear in generated code
#[allow(clippy::all)]
pub mod parsing;
pub mod pushdown;
pub mod runtime;
mod string_constants;
#[cfg(test)]
mod test_string_constants;
pub mod types;
mod uninit;
pub mod awk_util;

use clap::{Arg, Command};

use arena::Arena;
use cfg::Escaper;
use codegen::intrinsics::IntoRuntime;
use common::{CancelSignal, ExecutionStrategy, Stage};
use runtime::{
    splitter::{
        batch::{ByteReader, CSVReader, InputFormat},
        jsonl::JsonlReader,
        markdown::MarkdownCsv,
        parquet::ParquetJson,
        regex::RegexSplitter,
    },
    ChainedReader, LineReader, CHUNK_SIZE,
};
use std::fs::{File, Permissions};
use std::io::{self, BufReader, Write};
use std::iter::once;
use std::mem;

#[cfg(target_family = "unix")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Exit codes follow gawk: 1 for usage and syntax errors (`fail!`), 2 for other fatal errors
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
        std::process::exit(2)
    }}
}

#[derive(Clone)]
struct PreludeScalars {
    arbitrary_shell: bool,
    fold_regexes: bool,
    parse_header: bool,
    header_per_file: bool,
    escaper: Escaper,
    stage: Stage<()>,
}

struct RawPrelude {
    argv: Vec<String>,
    var_decs: Vec<String>,
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
    // (all transparently decompressed), standard input (the `-` operand), or nothing (skipped
    // operands).
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
            name => {
                if std::fs::metadata(name).map_or(false, |m| m.is_dir()) {
                    eprintln_ignore!(
                        "zawk: warning: command line argument `{}' is a directory: skipped",
                        name
                    );
                    return Ok(Source::Empty);
                }
                let file = File::open(name)
                    .map_err(|e| common::InputOpenError::new_io(name, &e))?;
                runtime::decompress::maybe_decompress(name, file).map(Source::File)
            }
        }
    }))
}

fn chained<LR: LineReader>(lr: LR) -> ChainedReader<LR> {
    ChainedReader::new(once(lr))
}

fn get_vars<'a, 'b>(
    vars: impl Iterator<Item=&'b str>,
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
            fail!(
                "received -v flag without an '=' sign: {} (split_buf={:?})",
                var,
                split_buf
            );
        }
        let ident = a.alloc_str(split_buf[0].trim());
        if !lexer::is_ident(ident) {
            fatal!(
                "invalid identifier for left-hand side of -v flag: {}",
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
                a.alloc_slice(&[&*val]),
            ))
        };
        res.push((ident, &*val))
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
    }
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
    let stmt = match parser.parse(a, &mut buf, &mut prog, lexer) {
        Ok(()) => {
            prog.field_sep = prelude.field_sep;
            prog.prelude_vardecs = prelude.var_decs;
            prog.output_sep = prelude.output_sep;
            prog.output_record_sep = prelude.output_record_sep;
            prog.parse_header = prelude.scalars.parse_header;
            prog.header_per_file = prelude.scalars.header_per_file;
            a.alloc(prog)
        }
        Err(e) => {
            fail!("{}", parsing::syntax_error(text, &e));
        }
    };
    match cfg::ProgramContext::from_prog(a, stmt, prelude.scalars.escaper) {
        Ok(mut ctx) => {
            ctx.allow_arbitrary_commands = prelude.scalars.arbitrary_shell;
            ctx.fold_regex_constants = prelude.scalars.fold_regexes;
            ctx
        }
        Err(e) => fatal!("{}", parsing::render_error(&e)),
    }
}

fn run_interp_with_context<'a>(
    mut ctx: cfg::ProgramContext<'a, &'a str>,
    stdin: impl LineReader,
    ff: impl runtime::writers::FileFactory,
    num_workers: usize,
) {
    let rc = {
        let mut interp = match compile::bytecode(&mut ctx, stdin, ff, num_workers) {
            Ok(ctx) => ctx,
            Err(e) => fatal!("{}", parsing::render_error(&e)),
        };
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
fn get_matches(
    app: clap::Command,
    args: impl IntoIterator<Item = String>,
) -> clap::ArgMatches {
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

/// As in gawk, `-E file` is the last option: insert `--` after it so that clap treats all the
/// remaining arguments (even ones like `-v` or `--help`) as input files. This is what makes
/// `#!/usr/bin/env -S zawk -E` scripts receive their own arguments untouched.
fn end_options_after_exec(mut args: Vec<String>) -> Vec<String> {
    // Short options that take a value, either attached (`-F,`) or as the next argument.
    const SHORT_WITH_VALUE: &str = "efEOvFBoipj";
    const LONG_WITH_VALUE: &[&str] = &[
        "--source", "--program-file", "--exec", "--opt-level", "--out-file", "--input-format",
        "--field-separator", "--backend", "--output-format", "--chunk-size",
    ];
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" || arg == "-" || !arg.starts_with('-') {
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

/// `zawk lint`: prints the JSON report and exits with 1 if the program has errors (warnings do
/// not count) or a program file cannot be read.
fn run_lint(matches: &clap::ArgMatches) -> ! {
    let mut sources: Vec<(usize, lint::Source)> = Vec::new();
    if let (Some(idx), Some(vals)) = (matches.indices_of("program-file"), matches.get_many::<String>("program-file")) {
        for (i, file) in idx.zip(vals) {
            let text = if file == "-" {
                io::read_to_string(io::stdin())
            } else {
                std::fs::read_to_string(file)
            };
            match text {
                Ok(text) => sources.push((i, lint::Source { name: Some(file.clone()), text })),
                Err(e) => {
                    println!("{}", lint::file_error(file, &common::io_error_reason(&e)));
                    std::process::exit(1)
                }
            }
        }
    }
    if let (Some(idx), Some(vals)) = (matches.indices_of("source"), matches.get_many::<String>("source")) {
        sources.extend(idx.zip(vals).map(|(i, text)| (i, lint::Source { name: None, text: text.clone() })));
    }
    sources.sort_by_key(|(i, _)| *i);
    let mut sources: Vec<lint::Source> = sources.into_iter().map(|(_, s)| s).collect();
    // As for a run, `-e` sources are named `-e#N` when the program has more than one source.
    if sources.len() > 1 {
        let mut inline_count = 0;
        for src in sources.iter_mut().filter(|s| s.name.is_none()) {
            inline_count += 1;
            src.name = Some(parsing::inline_source_name(inline_count));
        }
    }
    if sources.is_empty() {
        let text = match matches.get_one::<String>("program") {
            // As for a run, sugar syntax only applies to a program operand.
            Some(p) if matches.get_flag("posix") => p.clone(),
            Some(p) => awk_util::sugar_syntax_convert(p.trim().to_string()),
            None => match io::read_to_string(io::stdin()) {
                Ok(text) => text,
                Err(e) => fatal!("can't read program from stdin: {}", e),
            },
        };
        sources.push(lint::Source { name: None, text });
    }
    let (errors, _, report) = lint::lint(&sources, &lint_options(matches));
    println!("{}", report);
    std::process::exit(if errors == 0 { 0 } else { 1 })
}

/// `zawk --lint`: prints the JSON report to stderr when the program has diagnostics, and exits
/// with 1 without running the program when it has errors.
fn lint_before_run(sources: &[lint::Source], matches: &clap::ArgMatches) {
    let (errors, warnings, report) = lint::lint(sources, &lint_options(matches));
    if errors + warnings > 0 {
        eprintln!("{}", report);
    }
    if errors > 0 {
        std::process::exit(1)
    }
}

/// The lint options given by the `-A`, `-p` and `-v` arguments of `zawk lint` or of a run.
fn lint_options(matches: &clap::ArgMatches) -> lint::Options {
    lint::Options {
        allow_arbitrary_commands: matches.get_flag("arbitrary-shell"),
        parallel: match matches.get_one::<String>("parallel-strategy").map(|s| s.as_str()) {
            Some("r") | Some("record") => Some(lint::Parallel::Record),
            Some("f") | Some("file") => Some(lint::Parallel::File),
            _ => None,
        },
        assigned: matches
            .get_many::<String>("var")
            .into_iter()
            .flatten()
            .map(|v| v.split_once('=').map_or(v.as_str(), |(n, _)| n).trim().to_string())
            .collect(),
    }
}

const DEFAULT_OPT_LEVEL: i32 = 3;

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
    //.env load support
    dotenvy::dotenv().ok();
    runtime::logging::init();
    let dump_cmd = Command::new("dump").about("Dump text to CSV")
        .arg(Arg::new("prometheus")
            .long("prometheus")
            .num_args(0)
            .help("Parse Prometheus metrics to CSV")
        )
        .arg(Arg::new("input-file")
            .index(1)
            .required(true)
            .help("Text file or URL to parse")
        );
    let init_cmd = Command::new("init").about("Create a new AWK file with help info")
        .arg(Arg::new("awk-file")
            .index(1)
            .required(true)
            .help("AWK file to create")
        );
    let functions_cmd = Command::new("functions").about("List builtin functions with signatures, descriptions and examples")
        .arg(Arg::new("json")
            .long("json")
            .num_args(0)
            .help("Output as JSON: name, category, signature, params, returns, summary and example")
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
        .arg(Arg::new("arbitrary-shell")
            .short('A')
            .long("arbitrary-shell")
            .num_args(0)
            .help("Allow user input in commands run by pipes and system(), as with `zawk -A`"))
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
        .arg(Arg::new("program")
            .help("The program text to lint; read from stdin when no program is given"));
    let app = Command::new("zawk")
        .version(builtins::VERSION)
        .author("Eli R, linux_china")
        .about("zawk is an AWK language implementation by Rust with stdlib support")
        .subcommand(dump_cmd)
        .subcommand(init_cmd)
        .subcommand(functions_cmd)
        .subcommand(lint_cmd)
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
        .arg(Arg::new("characters-as-bytes")
            .long("characters-as-bytes")
            .short('b')
            .num_args(0)
            .help("Treat all input data as single-byte characters: length(), substr(), index(), match() and printf widths count bytes"))
        .arg(Arg::new("posix")
            .long("posix")
            .num_args(0)
            .help("POSIX compatibility: disable zawk's command-line sugar syntax (e.g. `zawk '$1,$2'`, `zawk '/re/'`). zawk extension functions remain available"))
        .arg(Arg::new("lint")
            .long("lint")
            .num_args(0)
            .help("Lint the program before running it, as `zawk lint` does: the JSON report is printed to stderr when there are diagnostics, and the program does not run when there are errors"))
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
            .value_name("csv|tsv|jsonl|parquet|markdown")
            .conflicts_with("field-separator")
            .help("Input is split according to the rules of (csv|tsv|jsonl|parquet|markdown). $0 contains the unescaped line. Assigning to columns does nothing. jsonl implies -H: `FI` maps JSON keys of the first record to columns. parquet implies -H: `FI` maps column names to columns, $0 is the row as a JSON object. markdown (md) reads the first table as CSV and implies -H: `FI` maps column names (without `:TYPE` suffixes) to columns")
            .value_parser(["csv", "tsv", "jsonl", "ndjson", "parquet", "markdown", "md"]))
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
            .value_name("csv|tsv|markdown|parquet")
            .help("If set, records output via print are escaped according to the rules of the corresponding format. markdown (md) renders stdout as a Markdown table: the first record is the header row, followed by the delimiter row and the data rows. parquet writes stdout as a Parquet file: the first record is the header, a column name may end with :TYPE (BOOLEAN, INT8, INT16, INT32, INT64, FLOAT, DOUBLE, DATE, TIME, TIMESTAMP, JSON, UUID, STRING; default STRING), followed by the data rows")
            .value_parser(["csv", "tsv", "markdown", "md", "parquet"]))
        .arg(Arg::new("program")
            .index(1)
            .help("The zawk program to execute"))
        .arg(Arg::new("input-files")
            .index(2)
            .num_args(1..)
            .help("Input files to be read by zawk program"))
        .arg(Arg::new("parallel-strategy")
            .short('p')
            .help("Attempt to execute the script in parallel. Strategy r[ecord] parallelizes within the current input file. Strategy f[ile] parallelizes between input files")
            .value_parser(["r", "record", "f", "file"]))
        .arg(Arg::new("chunk-size")
            .long("chunk-size")
            .num_args(1)
            .help("Buffer size when reading input. This is present primarily for debugging purposes; it's possible that tuning this will help performance, but it should not be necessary"))
        .arg(Arg::new("arbitrary-shell")
            .short('A')
            .long("arbitrary-shell")
            .num_args(0)
            .help("By default, strings that are passed to the shell via pipes or the 'system' function are restricted from potentially containing user input. This flag bypasses that check, for the cases where such a use is known to be safe"))
        .arg(Arg::new("jobs")
            .short('j')
            .requires("parallel-strategy")
            .num_args(1)
            .help("Number or worker threads to launch when executing in parallel, requires '-p' flag to be set. When using record-level parallelism, this value is an upper bound on the number of worker threads that will be spawned; the number of active worker threads is chosen dynamically"));
    // display help/version information from awk file
    let mut args: Vec<String> = std::env::args().collect();
    if args.len() > 2 { // sub help from
        let last_pair = args.last().unwrap();
        if last_pair == "--help" || last_pair == "-h" || last_pair == "--version" || last_pair == "-v" {
            let awk_file_supplied = args.iter().any(|item| item == "-f" || item.starts_with("--program-file")
                || item == "-E" || item.starts_with("--exec"));
            if awk_file_supplied {
                let last_pair = last_pair.clone();
                args.remove(args.len() - 1); // remove --help and --version
                let matches = get_matches(app, args);
                let awk_file = matches.get_one::<String>("exec")
                    .or_else(|| matches.get_one::<String>("program-file"))
                    .unwrap();
                if last_pair.contains("-h") {
                    awk_util::print_awk_file_help(awk_file);
                } else {
                    awk_util::print_awk_file_version(awk_file);
                }
                return;
            }
        }
    }
    let matches = get_matches(app, end_options_after_exec(std::env::args().collect()));
    // dump sub command
    if let Some(matches) = matches.subcommand_matches("dump") {
        let input_file = matches.get_one::<String>("input-file").unwrap();
        if matches.get_flag("prometheus") {
            let text = runtime::csv::parse_prometheus(input_file);
            println!("{}", text);
        }
        return;
    }
    // functions sub command
    if let Some(matches) = matches.subcommand_matches("functions") {
        let docs = function_docs::find(matches.get_one::<String>("filter").map(|s| s.as_str()));
        if matches.get_flag("json") {
            println!("{}", function_docs::render_json(&docs));
        } else {
            print!("{}", function_docs::render_text(&docs));
        }
        return;
    }
    // lint sub command
    if let Some(matches) = matches.subcommand_matches("lint") {
        run_lint(matches);
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
    let ifmt = match matches.get_one::<String>("input-format").map(|s| s.as_str()) {
        Some("csv") => Some(InputFormat::CSV),
        Some("tsv") => Some(InputFormat::TSV),
        Some("jsonl") | Some("ndjson") | Some("parquet") | Some("markdown") | Some("md") => None,
        Some(x) => fail!("invalid input format: {}", x),
        None => None,
    };
    let mut exec_strategy = match matches.get_one::<String>("parallel-strategy").map(|s| s.as_str()) {
        Some("r") | Some("record") => ExecutionStrategy::ShardPerRecord,
        Some("f") | Some("file") => ExecutionStrategy::ShardPerFile,
        None => ExecutionStrategy::Serial,
        Some(x) => fail!(
            "invalid execution strategy (clap arg parsing should handle this): {}",
            x
        ),
    };

    // NB: do we want this to be a command-line param?
    let chunk_size = if let Some(cs) = matches.get_one::<String>("chunk-size") {
        match cs.parse::<usize>() {
            Ok(u) => u,
            Err(e) => fail!("value of 'chunk-size' flag must be numeric: {}", e),
        }
    } else {
        CHUNK_SIZE
    };
    let mut num_workers = match matches.get_one::<String>("jobs") {
        Some(s) => match s.parse::<usize>() {
            Ok(u) => u,
            Err(e) => fail!("value of 'jobs' flag must be numeric: {}", e),
        },
        None => exec_strategy.num_workers(),
    };
    // As in gawk, ARGV[0] is the name zawk was invoked with, without its directory.
    let program_name = std::env::args()
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
        .unwrap_or_else(Vec::new);
    // get awk program code
    // `-f`, `-E` and `-e` sources, in command-line order.
    let mut sources: Vec<(usize, bool, &String)> = Vec::new();
    for (id, is_file) in [("program-file", true), ("exec", true), ("source", false)] {
        if let (Some(idx), Some(vals)) = (matches.indices_of(id), matches.get_many::<String>(id)) {
            sources.extend(idx.zip(vals).map(|(i, v)| (i, is_file, v)));
        }
    }
    sources.sort_by_key(|(i, _, _)| *i);
    // Where each source starts in the program, to report errors as `file:line:column`.
    let mut source_map = parsing::SourceMap::default();
    // The same sources, for `--lint`.
    let mut lint_sources: Vec<lint::Source> = Vec::new();
    let program_code = {
        if !sources.is_empty() {
            // We specified the program with options, so the "program" will be
            // interpreted as another input file.
            if let Some(p) = matches.get_one::<String>("program") {
                input_files.insert(0, p.into());
            }
            let mut prog = String::new();
            // A lone `-e` is reported as `line 1, column 7`; with other sources, each `-e` is
            // named `-e#N` so that its lines are not taken for lines of another source.
            let named = sources.len() > 1;
            let mut inline_count = 0;
            for (_, is_file, prog_file) in sources {
                let name = if is_file {
                    Some(prog_file.clone())
                } else {
                    inline_count += 1;
                    named.then(|| parsing::inline_source_name(inline_count))
                };
                source_map.push(name.clone(), prog.matches('\n').count());
                if !is_file {
                    lint_sources.push(lint::Source { name, text: prog_file.clone() });
                    prog.push_str(prog_file);
                    prog.push('\n');
                    continue;
                }
                if prog_file.starts_with("https://") || prog_file.starts_with("http://") {
                    // Error pages (404, proxy errors, ...) must not be run as programs.
                    match reqwest::blocking::get(prog_file)
                        .and_then(|resp| resp.error_for_status())
                        .and_then(|resp| resp.text())
                    {
                        Ok(p) => {
                            prog.push_str(p.as_str());
                            prog.push('\n');
                            lint_sources.push(lint::Source { name, text: p });
                        }
                        Err(e) => fatal!("can't open source file `{}' for reading: {}", prog_file, e),
                    }
                } else {
                    match std::fs::read_to_string(prog_file) {
                        Ok(p) => {
                            prog.push_str(p.as_str());
                            prog.push('\n');
                            lint_sources.push(lint::Source { name, text: p });
                        }
                        Err(e) => fatal!(
                            "can't open source file `{}' for reading: {}",
                            prog_file,
                            common::io_error_reason(&e)
                        ),
                    }
                }
            }
            prog
        } else if let Some(p) = matches.get_one::<String>("program") {
            // Sugar syntax only applies to programs given on the command line, not to `-f` files.
            if matches.get_flag("posix") {
                p.clone()
            } else {
                awk_util::sugar_syntax_convert(String::from(p.trim()))
            }
        } else {
            fail!("must specify program at command line, with -e, or in a file via -f");
        }
    };
    if matches.get_flag("lint") {
        if lint_sources.is_empty() {
            lint_sources.push(lint::Source { name: None, text: program_code.clone() });
        }
        lint_before_run(&lint_sources, &matches);
    }
    parsing::set_program(&program_code, source_map);
    // Build ARGV from the final input files: with `-f`, the first operand is parsed by clap
    // as `program` and only gets added to `input_files` above.
    let argv: Vec<String> = std::iter::once(program_name)
        .chain(input_files.iter().cloned())
        .collect();
    let (escaper, output_sep, output_record_sep) = match matches.get_one::<String>("output-format").map(|s| s.as_str()) {
        Some("csv") => (Escaper::CSV, Some(","), Some("\n")),
        Some("tsv") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and rendered as table rows by `MarkdownFactory`.
        Some("markdown") | Some("md") => (Escaper::TSV, Some("\t"), Some("\n")),
        // Records are written as TSV and converted to a Parquet file by `ParquetFactory`.
        Some("parquet") => (Escaper::TSV, Some("\t"), Some("\n")),
        Some(s) => fail!(
            "invalid output format {:?}; expected csv, tsv, markdown or parquet (or the empty string)",
            s
        ),
        None => (Escaper::Identity, None, None),
    };
    let arbitrary_shell = matches.get_flag("arbitrary-shell");
    runtime::str_impl::set_characters_as_bytes(matches.get_flag("characters-as-bytes"));
    let jsonl = matches!(
        matches.get_one::<String>("input-format").map(|s| s.as_str()),
        Some("jsonl") | Some("ndjson")
    );
    let parquet = matches!(
        matches.get_one::<String>("input-format").map(|s| s.as_str()),
        Some("parquet")
    );
    let markdown = matches!(
        matches.get_one::<String>("input-format").map(|s| s.as_str()),
        Some("markdown") | Some("md")
    );
    // JSON Lines records, Parquet files and Markdown tables are self-describing, so FI is always
    // populated from the first record (the column names for Parquet and Markdown tables).
    let parse_header = matches.get_flag("parse-header") || jsonl || parquet || markdown;
    // Every input file starts with a header line (JSON Lines and Parquet map each record by key).
    let header_per_file = parse_header && !jsonl && !parquet;
    let multi_file_headers = header_per_file && input_files.len() > 1;
    if multi_file_headers && !matches!(exec_strategy, ExecutionStrategy::Serial) {
        // The workers cannot tell which record is the header of a file.
        eprintln!("zawk: warning: -H with several input files: running serially");
        exec_strategy = ExecutionStrategy::Serial;
        num_workers = 1;
    }

    let opt_level: i32 = match matches.get_one::<String>("opt-level").map(|s| s.as_str()) {
        Some("3") => 3,
        Some("2") => 2,
        Some("1") => 1,
        Some("0") => 0,
        Some("-1") => -1,
        None => DEFAULT_OPT_LEVEL,
        Some(x) => panic!("this case should be covered by clap argument validation: found unexpected opt-level value {}", x),
    };
    let raw = RawPrelude {
        field_sep: matches.get_one::<String>("field-separator").map(String::from),
        var_decs: matches
            .get_many::<String>("var")
            .map(|x| x.map(String::from).collect())
            .unwrap_or_else(Vec::new),
        output_sep,
        scalars: PreludeScalars {
            escaper,
            arbitrary_shell,
            fold_regexes: opt_level >= 3,
            stage: exec_strategy.stage(),
            parse_header,
            header_per_file,
        },
        output_record_sep,
        argv,
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
    runtime::set_multi_file_headers(multi_file_headers);
    let check_utf8 = matches.get_flag("utf8");
    let signal = CancelSignal::default();

    // This horrid macro is here because all the different ways of reading input are different
    // types, making functions hard to write. Still, there must be something to be done to clean
    // this up here.
    macro_rules! with_inp {
        ($analysis:expr, $inp:ident, $body:expr) => {{
            if markdown {
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
                );
                $body
            } else if parquet {
                // Parquet rows are read as JSON Lines.
                let fi_names = awk_util::fi_constant_keys(&program_code);
                let sources: Vec<(ParquetJson, String)> = if input_files.len() == 0 {
                    vec![(ParquetJson::stdin(), String::from("-"))]
                } else {
                    for file in input_files.iter().filter(|f| f.as_str() != "-") {
                        if let Err(e) = ParquetJson::check(file) {
                            fatal!("{}", e);
                        }
                    }
                    input_files
                        .iter()
                        .map(|file| match file.as_str() {
                            "-" => (ParquetJson::stdin(), file.clone()),
                            _ => (ParquetJson::file(file.clone()), file.clone()),
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
                        } else {
                            let $inp =
                                chained(RegexSplitter::new(_reader, chunk_size, "-", check_utf8));
                            $body
                        }
                    }
                    (None, cfg::SepAssign::Unsure) => {
                        let $inp =
                            chained(RegexSplitter::new(_reader, chunk_size, "-", check_utf8));
                        $body
                    }
                }
            } else if let Some(ifmt) = ifmt {
                let file_handles: Vec<_> = input_files
                    .iter()
                    .cloned()
                    .map(|file| (open_file_read(file.as_str()), file))
                    .collect();
                let $inp = CSVReader::new(
                    file_handles.into_iter(),
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
                            if field_sep == b" " && record_sep == b"\n" {
                                let $inp = ByteReader::new_whitespace(
                                    file_handles.into_iter(),
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            } else {
                                let $inp = ByteReader::new(
                                    file_handles.into_iter(),
                                    field_sep[0],
                                    record_sep[0],
                                    chunk_size,
                                    check_utf8,
                                    exec_strategy,
                                    signal.clone(),
                                );
                                $body
                            }
                        } else {
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
    let passed = awk_util::validate_awk_code(&program_code, &raw.var_decs);
    if !passed {
        std::process::exit(1);
    }
    let a = Arena::default();
    let ctx = get_context(program_code.as_str(), &a, get_prelude(&a, &raw));
    let analysis_result = ctx.analyze_sep_assignments();
    let out_file = matches.get_one::<String>("out-file");
    // stdout wrappers for output formats that are not escaped text.
    let output_format = match matches.get_one::<String>("output-format").map(|s| s.as_str()) {
        Some("markdown") | Some("md") => "markdown",
        Some("parquet") => "parquet",
        _ => "",
    };
    if output_format == "parquet" && out_file.is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        fatal!("refusing to write parquet output to a terminal; use --out-file FILE or redirect stdout");
    }
    macro_rules! with_out {
        ($factory:expr, |$inp:ident, $out:ident| $body:expr) => {
            match output_format {
                "markdown" => {
                    let $out = runtime::writers::MarkdownFactory($factory);
                    with_inp!(analysis_result, $inp, $body);
                }
                "parquet" => {
                    let $out = runtime::parquet_writer::ParquetFactory($factory);
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
                    runtime::writers::factory_from_file(oup)
                        .unwrap_or_else(|e| fatal!("failed to open {}: {}", oup, e)),
                    |$inp, $out| $body
                ),
                None => with_out!(runtime::writers::default_factory(), |$inp, $out| $body),
            }
        };
    }
    match matches.get_one::<String>("backend").map(|s| s.as_str()) {
        Some("interp") => {
            with_io!(|inp, oup| run_interp_with_context(ctx, inp, oup, num_workers))
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
