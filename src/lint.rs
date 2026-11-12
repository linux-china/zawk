//! `zawk lint`: checks a program without running it and reports the problems found as JSON.
//!
//! The program goes through the same stages as a real run (lexing, parsing, building the CFG and
//! type checking), but every error becomes a diagnostic with a code, a location and the line of
//! source it points at, instead of a message on stderr. A token scan also reports gawk features
//! that zawk accepts but ignores (e.g. `FIELDWIDTHS`), which a run would silently get wrong, and
//! a walk over the syntax tree reports likely mistakes that a run accepts silently: variables
//! that are read but never set, `printf` formats with too few arguments, calls with more
//! arguments than the function has parameters and, for `-p`, code whose meaning changes when it
//! runs in parallel.
//!
//! ```json
//! {
//!   "valid": false,
//!   "errors": 1,
//!   "warnings": 0,
//!   "diagnostics": [{
//!     "code": "E101", "severity": "error", "message": "unexpected end of program",
//!     "file": "demo.awk", "line": 3, "column": 1, "source": "BEGIN {", "caret": "^"
//!   }]
//! }
//! ```

use crate::arena::Arena;
use crate::ast;
use crate::builtins::Function;
use crate::cfg::{self, Escaper};
use crate::common::Stage;
use crate::common::{Either, FileSpec};
use crate::compile;
use crate::inspect::Sample;
use crate::lexer::{self, Loc, Tok};
use crate::parsing;
use lalrpop_util::ParseError;
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Value, json};

/// A part of the program: a `-f` file, a `-e` string or the program operand, or a piece of one
/// of them between `@include` lines (see `include`).
#[derive(Debug, Default, Clone)]
pub struct Source {
    /// The file name, or `None` for a program given on the command line.
    pub name: Option<String>,
    pub text: String,
    /// The line of its file the text starts at (0-based): not 0 for a piece after an `@include`.
    pub line: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Severity {
    Error,
    Warning,
}

struct Diagnostic {
    code: &'static str,
    severity: Severity,
    message: String,
    /// The location in the concatenated program, if known.
    loc: Option<Loc>,
}

/// How the program will be run.
#[derive(Default, Clone)]
pub struct Options {
    /// `-p`: the program runs in parallel.
    pub parallel: Option<Parallel>,
    /// Variables set on the command line with `-v`.
    pub assigned: Vec<String>,
    /// `-H` (or an input format that implies it): `FI` maps the column names of the header.
    pub header: bool,
    /// `-F` (or `-v FS=...`), escapes processed.
    pub field_sep: Option<String>,
    /// The first records of an input file, for the data-aware checks.
    pub data: Option<Data>,
}

/// A sample of an input file of the run (`zawk lint prog.awk data.csv`).
#[derive(Clone)]
pub struct Data {
    /// The file name, for the messages.
    pub file: String,
    pub sample: Sample,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Parallel {
    /// `-pr`: workers share the records of each file.
    Record,
    /// `-pf`: each worker reads whole files.
    File,
}

/// Lints `sources` as one program (they are concatenated, as for a run) and returns the JSON
/// report with whether the program is free of errors.
/// Returns the number of errors, the number of warnings and the JSON report.
pub fn lint(sources: &[Source], opts: &Options) -> (usize, usize, String) {
    // Each source is followed by a newline, as `main` does when it concatenates them.
    let mut prog = String::new();
    let mut starts = Vec::with_capacity(sources.len());
    for src in sources {
        starts.push(prog.matches('\n').count());
        prog.push_str(&src.text);
        prog.push('\n');
    }
    let mut diags = scan_tokens(&prog);
    // Variables declared with `# @var` come from `-v`, as given variables do.
    let mut assigned = opts.assigned.clone();
    assigned.extend(
        sources
            .iter()
            .flat_map(|s| crate::awk_util::declared_vars(&s.text)),
    );
    let opts = Options {
        assigned,
        ..opts.clone()
    };
    diags.extend(check_program(&prog, &opts));
    diags.sort_by_key(|d| d.loc.map_or((usize::MAX, 0), |l| (l.line, l.col)));

    let errors = diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let lines: Vec<&str> = prog.lines().collect();
    let items: Vec<Object> = diags
        .iter()
        .map(|d| to_json(d, &lines, sources, &starts))
        .collect();
    let warnings = diags.len() - errors;
    (errors, warnings, report(errors, warnings, &items))
}

/// The warnings a run in parallel `mode` prints to stderr before it starts: the variables
/// (`W302`) and array elements (`W303`) that END sees aggregated over the workers, which
/// otherwise give a wrong result without a word (e.g. a maximum summed over the workers).
/// Only the syntax tree is checked, so this costs one more parse of the program; a program that
/// does not parse gets no warning here (the run reports its error).
/// With `keep_order` (`--keep-order`, which promises the output of a serial run) the record
/// counters read in the main loop (`W301`) are reported too.
pub fn parallel_warnings(sources: &[Source], mode: Parallel, keep_order: bool) -> Vec<String> {
    let mut prog = String::new();
    let mut starts = Vec::with_capacity(sources.len());
    for src in sources {
        starts.push(prog.matches('\n').count());
        prog.push_str(&src.text);
        prog.push('\n');
    }
    let a = Arena::default();
    let text: &str = a.alloc_str(&prog);
    let mut buf = Vec::new();
    let mut program = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    if parser
        .parse(&a, &mut buf, &mut program, lexer::Tokenizer::new(text))
        .is_err()
    {
        return Vec::new();
    }
    let opts = Options {
        parallel: Some(mode),
        ..Options::default()
    };
    let mut diags: Vec<Diagnostic> = check_ast(text, &program, &opts)
        .into_iter()
        .filter(|d| {
            matches!(d.code, "W302" | "W303" | "W304" | "W305") || (keep_order && d.code == "W301")
        })
        .collect();
    diags.sort_by_key(|d| d.loc.map_or((usize::MAX, 0), |l| (l.line, l.col)));
    let lines: Vec<&str> = prog.lines().collect();
    let flag = match mode {
        _ if keep_order => "--keep-order",
        Parallel::Record => "-p r",
        Parallel::File => "-p f",
    };
    diags
        .iter()
        .map(|d| {
            let obj = to_json(d, &lines, sources, &starts);
            let field = |k: &str| obj.0.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
            let place = match (field("line"), field("column")) {
                (Some(line), Some(col)) => match field("file").and_then(|f| f.as_str().map(String::from)) {
                    Some(file) => format!("{}:{}:{}: ", file, line, col),
                    None => format!("line {}, column {}: ", line, col),
                },
                _ => String::new(),
            };
            format!(
                "zawk: warning: {}: {}{} {} (`--lint` lists them, `--deny-warnings` refuses to run)",
                flag, place, d.code, d.message
            )
        })
        .collect()
}

/// Whether the program has a range pattern (`start, end`), whose state a parallel run keeps per
/// worker. A program that does not parse has none here (the run reports its error).
pub fn has_range_pattern(sources: &[Source]) -> bool {
    let mut prog = String::new();
    for src in sources {
        prog.push_str(&src.text);
        prog.push('\n');
    }
    let a = Arena::default();
    let text: &str = a.alloc_str(&prog);
    let mut buf = Vec::new();
    let mut program = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    parser
        .parse(&a, &mut buf, &mut program, lexer::Tokenizer::new(text))
        .is_ok()
        && program
            .pats
            .iter()
            .any(|(p, _)| matches!(p, ast::Pattern::Comma(..)))
}

/// The report for a program file that cannot be read.
pub fn file_error(file: &str, reason: &str) -> String {
    let d = Object(vec![
        ("code", json!("E001")),
        ("severity", json!("error")),
        (
            "message",
            json!(format!("can't open source file for reading: {}", reason)),
        ),
        ("file", json!(file)),
    ]);
    report(1, 0, &[d])
}

/// The report for an `@include` (or `--include`) file that cannot be loaded.
pub fn include_error(message: &str) -> String {
    let d = Object(vec![
        ("code", json!("E001")),
        ("severity", json!("error")),
        ("message", json!(message)),
    ]);
    report(1, 0, &[d])
}

/// The report for an input file that cannot be read.
pub fn input_error(file: &str, reason: &str) -> String {
    let d = Object(vec![
        ("code", json!("E002")),
        ("severity", json!("error")),
        (
            "message",
            json!(format!("can't open input file for reading: {}", reason)),
        ),
        ("file", json!(file)),
    ]);
    report(1, 0, &[d])
}

fn report(errors: usize, warnings: usize, diagnostics: &[Object]) -> String {
    struct Report<'a>(usize, usize, &'a [Object]);
    impl Serialize for Report<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut map = s.serialize_map(Some(4))?;
            map.serialize_entry("valid", &(self.0 == 0))?;
            map.serialize_entry("errors", &self.0)?;
            map.serialize_entry("warnings", &self.1)?;
            map.serialize_entry("diagnostics", self.2)?;
            map.end()
        }
    }
    serde_json::to_string_pretty(&Report(errors, warnings, diagnostics)).unwrap()
}

/// A JSON object that keeps its keys in order (`serde_json::Map` sorts them), so that the
/// report reads `code`, `message`, ... before the source excerpt.
struct Object(Vec<(&'static str, Value)>);

impl Serialize for Object {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}

fn to_json(d: &Diagnostic, lines: &[&str], sources: &[Source], starts: &[usize]) -> Object {
    let mut v = Object(vec![
        ("code", json!(d.code)),
        (
            "severity",
            json!(match d.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            }),
        ),
        ("message", json!(d.message)),
    ]);
    let Some(loc) = d.loc else {
        return v;
    };
    // The source the line belongs to: the last one starting at or before it.
    let idx = starts.iter().rposition(|s| *s <= loc.line).unwrap_or(0);
    let line_text = lines.get(loc.line).copied().unwrap_or("");
    let prefix = line_text.get(..loc.col).unwrap_or(line_text);
    // Columns count characters, so that they match what an editor shows.
    let column = prefix.chars().count() + 1;
    let caret: String = prefix
        .chars()
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .chain(std::iter::once('^'))
        .collect();
    let file = sources.get(idx).and_then(|s| s.name.as_ref());
    let first_line = sources.get(idx).map_or(0, |s| s.line);
    v.0.extend([
        ("file", json!(file)),
        (
            "line",
            json!(loc.line - starts.get(idx).copied().unwrap_or(0) + first_line + 1),
        ),
        ("column", json!(column)),
        ("source", json!(line_text)),
        ("caret", json!(caret)),
    ]);
    v
}

/// Parses, builds and type checks the program, returning the warnings of the syntax tree checks
/// and the first error found.
fn check_program(prog: &str, opts: &Options) -> Vec<Diagnostic> {
    let a = Arena::default();
    let text: &str = a.alloc_str(prog);
    let mut buf = Vec::new();
    let mut program = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    if let Err(e) = parser.parse(&a, &mut buf, &mut program, lexer::Tokenizer::new(text)) {
        return vec![parse_diagnostic(text, &e)];
    }
    let mut diags = check_ast(text, &program, opts);
    diags.extend(compile_program(&a, text, program));
    diags
}

/// Builds and type checks a parsed program, returning the first error found.
fn compile_program<'a>(
    a: &'a Arena,
    text: &str,
    program: ast::Prog<'a, 'a, &'a str>,
) -> Option<Diagnostic> {
    let program = a.alloc(program);
    // The type checker reports some unsupported programs with a panic: report it as an error
    // instead of crashing (and keep the panic message off stderr).
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ctx = cfg::ProgramContext::from_prog(a, program, Escaper::Identity)?;
        compile::context_compiles(&mut ctx)
    }));
    std::panic::set_hook(hook);
    match res {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(compile_diagnostic(text, &e.0, e.1)),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown error".to_string());
            Some(Diagnostic {
                code: "E900",
                severity: Severity::Error,
                message: format!("internal compiler error: {}", msg),
                loc: None,
            })
        }
    }
}

fn parse_diagnostic(prog: &str, err: &ParseError<Loc, Tok, lexer::Error>) -> Diagnostic {
    let (code, loc) = match err {
        ParseError::InvalidToken { location } => ("E102", *location),
        ParseError::UnrecognizedEof { location, .. } => ("E101", *location),
        ParseError::UnrecognizedToken {
            token: (l, _, r), ..
        } => {
            if parsing::unsupported_field_operand(prog, l, r).is_some() {
                ("E201", *l)
            } else {
                ("E101", *l)
            }
        }
        ParseError::ExtraToken { token: (l, _, _) } => ("E101", *l),
        ParseError::User { error } if error.desc.contains("unsupported") => {
            ("E201", error.location)
        }
        ParseError::User { error } => ("E102", error.location),
    };
    // Reuse the message of a run, without its location and source excerpt.
    let (_, message) = parsing::describe_parse_error(prog, err);
    Diagnostic {
        code,
        severity: Severity::Error,
        message,
        loc: Some(loc),
    }
}

/// gawk builtin functions that zawk lacks, with what to use instead; calling one is an unsupported
/// feature rather than a typo.
const UNSUPPORTED_GAWK_FUNCTIONS: &[(&str, &str)] = &[
    (
        "bindtextdomain",
        "internationalization is not supported; drop the call",
    ),
    (
        "dcgettext",
        "internationalization is not supported; use the string itself",
    ),
    (
        "dcngettext",
        "internationalization is not supported; pick the singular or plural string with `n == 1 ? s1 : s2`",
    ),
];

fn compile_diagnostic(prog: &str, msg: &str, loc: Option<Loc>) -> Diagnostic {
    let msg = msg
        .trim_start_matches("zawk: ")
        .trim_start_matches("fatal: ")
        .trim_start_matches("syntax error: ");
    // Debug builds prefix messages with where zawk raised them: `[src/cfg.rs:1:2] `.
    let msg = match msg.strip_prefix("[src/").and_then(|m| m.split_once("] ")) {
        Some((_, m)) => m,
        None => msg,
    };
    let name = quoted_name(msg);
    let (code, call) = if msg.starts_with("Call to unknown function") {
        match name {
            Some(n) if UNSUPPORTED_GAWK_FUNCTIONS.iter().any(|(f, _)| *f == n) => ("E203", true),
            _ => ("E301", true),
        }
    } else if msg.contains("arrays of arrays") {
        ("E202", false)
    } else if msg.contains(" arguments, got ") || msg.contains(" argument, got ") {
        ("E302", true)
    } else if msg.contains("as an array") || msg.contains("scalar context") {
        ("E303", false)
    } else if msg.contains("duplicates parameter")
        || msg.contains("function name as a parameter")
        || msg.contains("is used as a variable or an array")
    {
        ("E305", false)
    } else if msg.contains("must be a variable, an array element or a field") {
        ("E306", false)
    } else {
        ("E300", false)
    };
    let message = if code == "E203" {
        let hint = UNSUPPORTED_GAWK_FUNCTIONS
            .iter()
            .find(|(f, _)| Some(*f) == name)
            .map_or("", |(_, h)| h);
        format!("{}: not supported by zawk; {}", msg, hint)
    } else {
        msg.to_string()
    };
    // Without a location, point at the first use of the name the error mentions.
    let loc = loc.or_else(|| name.and_then(|n| find_name(prog, n, call)));
    let loc = loc.or_else(|| {
        if code == "E202" {
            find_array_of_arrays(prog)
        } else {
            None
        }
    });
    Diagnostic {
        code,
        severity: Severity::Error,
        message,
        loc,
    }
}

/// The first name quoted with `"..."` or `` `...` `` in a message.
fn quoted_name(msg: &str) -> Option<&str> {
    for q in ['"', '`'] {
        if let Some((_, rest)) = msg.split_once(q)
            && let Some((name, _)) = rest.split_once(if q == '`' { '`' } else { '"' })
                && !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return Some(name);
                }
    }
    None
}

/// The location of the first token naming `name`: a call `name(` if `call`, else any use.
fn find_name(prog: &str, name: &str, call: bool) -> Option<Loc> {
    lexer::Tokenizer::new(prog)
        .map_while(Result::ok)
        .find(|(_, tok, _)| match tok {
            Tok::CallStart(n) => *n == name,
            Tok::Ident(n) => !call && *n == name,
            _ => false,
        })
        .map(|(l, _, _)| l)
}

/// The location of the first `]` directly followed by `[`, as in `a[i][j]`.
fn find_array_of_arrays(prog: &str) -> Option<Loc> {
    let toks: Vec<_> = lexer::Tokenizer::new(prog).map_while(Result::ok).collect();
    toks.windows(2)
        .find(|w| matches!((&w[0].1, &w[1].1), (Tok::RBrack, Tok::LBrack)))
        .map(|w| w[1].0)
}

type AstExpr<'a> = ast::Expr<'a, 'a, &'a str>;
type AstStmt<'a> = ast::Stmt<'a, 'a, &'a str>;

/// The part of the program a statement belongs to; in parallel mode, the main loop (with
/// `BEGINFILE`, `ENDFILE` and `PREPARE`) runs in the workers.
#[derive(Clone, Copy, PartialEq)]
enum Region {
    Begin,
    Main,
    End,
    Function,
}

/// How a variable is set.
#[derive(Clone, Copy, PartialEq)]
enum SetKind {
    /// `x += e`, `x -= e`, `x = x + e`, `x++`: workers' values add up to the serial result.
    Sum,
    /// Any other assignment, e.g. `x = $1`.
    Other,
    /// Passed to a function that may fill it, deleted, or a `for (x in a)` key.
    Indirect,
}

struct Use<'a> {
    name: &'a str,
    region: Region,
    loc: Option<Loc>,
}

struct Set<'a> {
    name: &'a str,
    region: Region,
    kind: SetKind,
    loc: Option<Loc>,
}

/// What the syntax tree checks need to know about the program, collected in one walk.
struct Facts<'a, 'p> {
    locs: &'p ast::ExprLocs,
    region: Region,
    /// The parameters of the function being walked, which are local variables.
    locals: &'p [&'a str],
    uses: Vec<Use<'a>>,
    sets: Vec<Set<'a>>,
    /// Variables used as arrays.
    arrays: Vec<&'a str>,
    /// Calls of functions by name: the name, the number of arguments and the location.
    calls: Vec<(&'a str, usize, Option<Loc>)>,
    /// `printf` and `sprintf` with a literal format (or a concatenation of literals): the
    /// format, the number of arguments to format and the location.
    formats: Vec<(Vec<u8>, usize, Option<Loc>)>,
    /// `printf` with a format that reads a field and no arguments (`printf $0`): the location.
    dynamic_formats: Vec<Option<Loc>>,
    /// `$NF-1` (meaning `$(NF-1)`): the operator, the number and the location.
    field_offsets: Vec<(&'static str, i64, Option<Loc>)>,
    /// Arithmetic as an argument of a `dec_*` function (`dec_add(t, $3 * 1.06)`): the function
    /// name and the location of the call.
    decimal_float_args: Vec<(&'static str, Option<Loc>)>,
    /// Comparisons of a field with a numeric string (`$3 > "100"`): the field, the string and
    /// the location.
    string_comparisons: Vec<(String, &'a [u8], Option<Loc>)>,
    /// Assignments to `FS` in a main rule: the location.
    main_fs_sets: Vec<Option<Loc>>,
    /// String literals used as a condition (`if ("0")`): the string and the location.
    string_conds: Vec<(&'a [u8], Option<Loc>)>,
    /// Variables read into with `getline var`, often only to skip a line.
    getline_vars: Vec<&'a str>,
    /// Assignments to read-only variables and fields: the message and the location.
    read_only: Vec<(String, Option<Loc>)>,
    /// Reads of `FI[...]` (e.g. `$FI["name"]`): the column name, if it is a literal, and the
    /// location.
    header_columns: Vec<(Option<&'a [u8]>, Option<Loc>)>,
    /// Reads of fields with a literal number outside BEGIN (`$3`): the number and the location.
    fields: Vec<(i64, Option<Loc>)>,
    /// Comparisons of a field with a number (`$3 > 100`): the field, the number and the location.
    field_comparisons: Vec<(FieldRef<'a>, String, Option<Loc>)>,
    /// Whether the program reads a record with a plain `getline` outside BEGIN (which may have
    /// other fields than the sample).
    record_changed: bool,
    /// `FS = "..."` in BEGIN: the separators assigned.
    fs_literals: Vec<&'a [u8]>,
    /// The number of assignments to `FS` (of any kind, anywhere).
    fs_sets: usize,
    /// Reads of fields, `NF` and `FILENAME` in BEGIN: what is read and the location.
    begin_record_reads: Vec<(String, Option<Loc>)>,
    /// Whether BEGIN or a function reads a record with `getline`.
    getline: bool,
    /// The keys of the enclosing `for (k in a)` loops.
    keys: Vec<&'a str>,
    /// Comparisons of a `for (k in a)` key with a number: the key, the number and the location.
    key_comparisons: Vec<(&'a str, String, Option<Loc>)>,
    /// `print > 10`: output redirected to a file named by a number: the redirection, the
    /// file name (if it is a literal) and the location.
    numeric_redirects: Vec<(&'static str, Option<String>, Option<Loc>)>,
    /// Assignments used as a condition (`if (x = 1)`): the assigned variable (if it is one) and
    /// the location.
    cond_assigns: Vec<(Option<&'a str>, Option<Loc>)>,
    /// Calls of `index` with a regex literal as the first argument (`index(/re/, s)`, where
    /// `/re/` means `$0 ~ /re/`): the location.
    index_regexes: Vec<Option<Loc>>,
    /// String constants used as dynamic regexes (`s ~ "["`, `gsub("+", r)`) that do not compile,
    /// which fails the run: the pattern, why it is invalid and the location.
    string_regexes: Vec<(String, String, Option<Loc>)>,
    /// Bounds of `for (i in start..end)` that are strings, not numbers (`"a".."c"`): how the
    /// bound is written and the location.
    string_range_bounds: Vec<(String, Option<Loc>)>,
    /// Fields named by a string constant that is not a number (`$"age"`, which is `$0`): the
    /// string and the location.
    string_fields: Vec<(Vec<u8>, Option<Loc>)>,
    /// The names of the user-defined functions, which may read and write any global.
    user_funcs: Vec<&'a str>,
    /// Assignments to an array element whose result depends on whether the subscript or the
    /// right-hand side is evaluated first (`a[i++] = i`, `a[p(1)] = p(2)`): the location.
    order_dependent: Vec<Option<Loc>>,
    /// Assignments to array elements (`a[k] = v`, `a[k]++`): the array, how it is set and where.
    element_sets: Vec<Set<'a>>,
    /// Reads of array element values (`a[k]`, or the array passed to a user-defined function),
    /// as opposed to only its keys (`k in a`, `for (k in a)`, `length(a)`): the array and region.
    element_reads: Vec<(&'a str, Region)>,
}

/// What evaluating an expression may read and write, for spotting evaluation-order dependencies.
/// `"$0"` stands for the record and its fields.
#[derive(Default)]
struct Effects<'a> {
    reads: Vec<&'a str>,
    writes: Vec<&'a str>,
    /// Calls a user-defined function, which may read and write any global.
    calls: bool,
}

impl<'a> Effects<'a> {
    fn of(e: &'a AstExpr<'a>, user_funcs: &[&str]) -> Effects<'a> {
        let mut fx = Effects::default();
        fx.walk(e, user_funcs);
        fx
    }

    /// Records a write to the variable (or array) an assignment target names.
    fn target(&mut self, e: &'a AstExpr<'a>, user_funcs: &[&str]) {
        match e {
            ast::Expr::Var(name) => self.writes.push(name),
            ast::Expr::Index(arr, ix) => {
                self.target(arr, user_funcs);
                self.walk(ix, user_funcs);
            }
            ast::Expr::Unop(ast::Unop::Column, ix) => {
                self.writes.push("$0");
                self.walk(ix, user_funcs);
            }
            _ => self.walk(e, user_funcs),
        }
    }

    fn walk(&mut self, e: &'a AstExpr<'a>, user_funcs: &[&str]) {
        use ast::Expr::*;
        match e {
            ILit(_) | FLit(_) | StrLit(_) | ReadStdin | Cond(_) => {}
            PatLit(_) => self.reads.push("$0"),
            Var(name) => self.reads.push(name),
            Unop(ast::Unop::Column, x) => {
                self.reads.push("$0");
                self.walk(x, user_funcs);
            }
            Unop(_, x) => self.walk(x, user_funcs),
            Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) | Index(l, r) => {
                self.walk(l, user_funcs);
                self.walk(r, user_funcs);
            }
            ITE(c, t, f) => {
                self.walk(c, user_funcs);
                self.walk(t, user_funcs);
                self.walk(f, user_funcs);
            }
            Assign(lhs, rhs) | AssignOp(lhs, _, rhs) => {
                self.target(lhs, user_funcs);
                if matches!(e, AssignOp(..)) {
                    self.walk(lhs, user_funcs);
                }
                self.walk(rhs, user_funcs);
            }
            Inc { x, .. } => {
                self.target(x, user_funcs);
                self.walk(x, user_funcs);
            }
            Getline { into, from, .. } => {
                match into {
                    Some(into) => self.target(into, user_funcs),
                    None => self.writes.extend(["$0", "NF"]),
                }
                self.writes.push("NR");
                if let Some(from) = from {
                    self.walk(from, user_funcs);
                }
            }
            Call(f, args) => {
                let read_only = match f {
                    Either::Left(name) => {
                        if user_funcs.contains(name) {
                            self.calls = true;
                        }
                        READ_ONLY_FUNCTIONS.contains(name)
                    }
                    Either::Right(Function::Delete | Function::Clear) => false,
                    Either::Right(_) => true,
                };
                for arg in args.iter() {
                    match arg {
                        Var(name) if !read_only => self.writes.push(name),
                        _ => self.walk(arg, user_funcs),
                    }
                }
            }
        }
    }

    /// Whether evaluating `self` before or after `other` may change a result: one writes what
    /// the other reads or writes, or both call user functions.
    fn conflicts(&self, other: &Effects) -> bool {
        let touches = |w: &[&str], fx: &Effects| {
            w.iter()
                .any(|v| fx.reads.contains(v) || fx.writes.contains(v))
        };
        (self.calls && (other.calls || !other.writes.is_empty()))
            || (other.calls && !self.writes.is_empty())
            || touches(&self.writes, other)
            || touches(&other.writes, self)
    }
}

/// Special variables a program may read but not assign, with why.
const READ_ONLY_VARIABLES: &[(&str, &str)] = &[
    ("ARGC", "changing it does not change the input files"),
    ("ARGV", "changing it does not change the input files"),
    ("FILENAME", "it is the name of the input file being read"),
    ("NR", "it counts the records read"),
    ("NF", "it is the number of fields of the current record"),
    (
        "FI",
        "it maps the column names of the header to field indexes",
    ),
    (
        "MSG_SUBJECT",
        "it is the subject of the NATS message (or topic of the Kafka message) being read",
    ),
    (
        "MSG_HEADERS",
        "it holds the headers of the NATS or Kafka message being read",
    ),
    (
        "MSG_BODY",
        "it is the payload of the NATS or Kafka message being read",
    ),
];

/// A field read with a literal number or column name.
#[derive(Clone, Copy, PartialEq)]
enum FieldRef<'a> {
    Num(i64),
    Name(&'a [u8]),
}

impl FieldRef<'_> {
    fn of<'a>(e: &AstExpr<'a>) -> Option<FieldRef<'a>> {
        match e {
            ast::Expr::Unop(ast::Unop::Column, x) => match x {
                ast::Expr::ILit(n) => Some(FieldRef::Num(*n)),
                ast::Expr::Index(ast::Expr::Var("FI"), ast::Expr::StrLit(k)) => {
                    Some(FieldRef::Name(k))
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn describe(&self) -> String {
        match self {
            FieldRef::Num(n) => format!("${}", n),
            FieldRef::Name(k) => format!("$FI[\"{}\"]", String::from_utf8_lossy(k)),
        }
    }
}

/// Builtins that only read their arguments; a variable passed to any other function may be
/// filled by it (e.g. `split(s, parts)`, or an array passed to a user function).
const READ_ONLY_FUNCTIONS: &[&str] = &[
    "length", "substr", "index", "sprintf", "sin", "cos", "atan2", "exp", "log", "sqrt", "int",
    "srand", "tolower", "toupper", "system", "close", "fflush", "strtonum", "gensub", "and", "or",
    "xor", "compl", "lshift", "rshift", "strftime", "mktime", "trim", "ltrim", "rtrim", "max",
    "min", "abs", "uuid",
];

impl<'a, 'p> Facts<'a, 'p> {
    fn loc(&self, e: &AstExpr<'a>) -> Option<Loc> {
        self.locs.get(&ast::expr_key(e)).copied()
    }

    fn is_local(&self, name: &str) -> bool {
        self.locals.contains(&name)
    }

    fn used(&mut self, name: &'a str, loc: Option<Loc>) {
        if !self.is_local(name) {
            self.uses.push(Use {
                name,
                region: self.region,
                loc,
            });
        }
    }

    fn set(&mut self, name: &'a str, kind: SetKind, loc: Option<Loc>) {
        if !self.is_local(name) {
            if name == "FS" {
                self.fs_sets += 1;
            }
            self.sets.push(Set {
                name,
                region: self.region,
                kind,
                loc,
            });
        }
    }

    fn array(&mut self, e: &AstExpr<'a>) {
        if let ast::Expr::Var(name) = e
            && !self.is_local(name) && !self.arrays.contains(name) {
                self.arrays.push(name);
            }
    }

    fn stmt(&mut self, s: &'a AstStmt<'a>) {
        use ast::Stmt::*;
        match s {
            StartCond(_) | EndCond(_) | LastCond(_) | Break | Continue | Next | NextFile => {}
            Expr(e) => self.expr(e),
            Block(ss) => ss.iter().for_each(|s| self.stmt(s)),
            Print(args, out) => {
                args.iter().for_each(|e| self.expr(e));
                if let Some((e, spec)) = out {
                    self.check_numeric_redirect(e, *spec);
                    self.expr(e);
                }
            }
            Printf(fmt, args, out) => {
                match literal(fmt) {
                    Some(f) => self.formats.push((f, args.len(), self.loc(fmt))),
                    None if args.is_empty() && has_field(fmt) => {
                        self.dynamic_formats.push(self.loc(fmt))
                    }
                    None => {}
                }
                self.expr(fmt);
                args.iter().for_each(|e| self.expr(e));
                if let Some((e, spec)) = out {
                    self.check_numeric_redirect(e, *spec);
                    self.expr(e);
                }
            }
            If(c, t, f) => {
                self.condition(c);
                self.stmt(t);
                if let Some(f) = f {
                    self.stmt(f);
                }
            }
            For(init, cond, update, body) => {
                if let Some(s) = init {
                    self.stmt(s);
                }
                if let Some(e) = cond {
                    self.condition(e);
                }
                if let Some(s) = update {
                    self.stmt(s);
                }
                self.stmt(body);
            }
            DoWhile(c, body) | While(_, c, body) => {
                self.condition(c);
                self.stmt(body);
            }
            ForEach(key, arr, body) => {
                self.set(key, SetKind::Indirect, None);
                self.array(arr);
                self.expr(arr);
                self.keys.push(key);
                self.stmt(body);
                self.keys.pop();
            }
            ForRange(v, start, end, _, body) => {
                self.set(v, SetKind::Other, None);
                self.check_range_bound(start);
                self.check_range_bound(end);
                self.expr(start);
                self.expr(end);
                self.stmt(body);
            }
            Switch(e, cases) => {
                self.expr(e);
                for (label, body) in cases.iter() {
                    if let Some(l) = label {
                        self.expr(l);
                    }
                    self.stmt(body);
                }
            }
            Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            Abort(e) => self.expr(e),
            EndBlock(s) => self.stmt(s),
        }
    }

    /// Records an assignment to `e` if it is a read-only variable, an element of one, or a field.
    fn check_read_only(&mut self, e: &'a AstExpr<'a>) {
        let target = match e {
            ast::Expr::Index(arr @ ast::Expr::Var(_), _) => arr,
            _ => e,
        };
        match target {
            ast::Expr::Var(name) if !self.is_local(name) => {
                if let Some((_, why)) = READ_ONLY_VARIABLES.iter().find(|(n, _)| n == name) {
                    let msg = format!("`{}` is read-only and cannot be assigned: {}", name, why);
                    self.read_only.push((msg, self.loc(target)));
                }
            }
            ast::Expr::Unop(ast::Unop::Column, ix) => {
                let what = match ix {
                    ast::Expr::Index(ast::Expr::Var("FI"), _) => "`$FI[...]`",
                    ast::Expr::ILit(0) => "`$0`",
                    _ => "a field",
                };
                let msg = format!(
                    "fields are read-only: {} cannot be assigned; build the output in a variable instead, e.g. `line = $1 OFS $2`",
                    what
                );
                self.read_only.push((msg, self.loc(e)));
            }
            _ => {}
        }
    }

    /// Records `print > 10` (or `>>`): the file is named `10`; the author almost always meant
    /// to print a comparison.
    fn check_numeric_redirect(&mut self, e: &'a AstExpr<'a>, spec: FileSpec) {
        fn numeric(e: &AstExpr) -> bool {
            use ast::Binop::*;
            matches!(
                e,
                ast::Expr::ILit(_)
                    | ast::Expr::FLit(_)
                    | ast::Expr::Inc { .. }
                    | ast::Expr::Unop(ast::Unop::Neg | ast::Unop::Pos, _)
                    | ast::Expr::Binop(Plus | Minus | Mult | Div | Mod | Pow, _, _)
            )
        }
        let redirect = match spec {
            FileSpec::Trunc => ">",
            FileSpec::Append => ">>",
            FileSpec::Cmd => return,
        };
        if numeric(e) {
            let name = match e {
                ast::Expr::ILit(n) => Some(n.to_string()),
                ast::Expr::FLit(f) => Some(f.to_string()),
                _ => None,
            };
            self.numeric_redirects.push((redirect, name, self.loc(e)));
        }
    }

    /// Records a bound of `for (i in start..end)` that is a string (`"a"`, `x "y"`, `/re/`): it
    /// becomes an integer, 0 unless it starts with a number, so `"a".."c"` runs no iteration.
    fn check_range_bound(&mut self, e: &'a AstExpr<'a>) {
        let what = match e {
            ast::Expr::StrLit(s) if !starts_numeric(s) => {
                format!("the string \"{}\"", String::from_utf8_lossy(s))
            }
            ast::Expr::Binop(ast::Binop::Concat, _, _) => "a concatenation, a string,".to_string(),
            ast::Expr::PatLit(_) => "a regex literal (`$0 ~ /re/`, 0 or 1)".to_string(),
            _ => return,
        };
        self.string_range_bounds.push((what, self.loc(e)));
    }

    /// Walks a condition, recording assignments (`if (x = 1)`) and string literals
    /// (`if ("0")`) used as its value, also under `!`, `&&` and `||`.
    fn condition(&mut self, e: &'a AstExpr<'a>) {
        fn find<'a>(e: &'a AstExpr<'a>, found: &mut Vec<&'a AstExpr<'a>>) {
            match e {
                ast::Expr::Assign(..) | ast::Expr::StrLit(_) => found.push(e),
                ast::Expr::Unop(ast::Unop::Not, x) => find(x, found),
                ast::Expr::And(l, r) | ast::Expr::Or(l, r) => {
                    find(l, found);
                    find(r, found);
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        find(e, &mut found);
        for a in found {
            // Assignments to fields and to non-lvalues are errors reported elsewhere.
            let (name, lhs) = match a {
                ast::Expr::StrLit(s) => {
                    self.string_conds.push((s, self.loc(a)));
                    continue;
                }
                ast::Expr::Assign(lhs @ ast::Expr::Var(name), _) => (Some(*name), lhs),
                ast::Expr::Assign(lhs @ ast::Expr::Index(..), _) => (None, lhs),
                _ => continue,
            };
            let loc = self.loc(a).or_else(|| self.loc(lhs));
            self.cond_assigns.push((name, loc));
        }
        self.expr(e);
    }

    /// Records `k > 9` (either way round) where `k` is the key of an enclosing `for (k in a)`:
    /// array keys are strings, so gawk compares them as strings ("10" < "9").
    fn check_key_comparison(&mut self, l: &'a AstExpr<'a>, r: &'a AstExpr<'a>) {
        let (var, x) = match (l, r) {
            (v @ ast::Expr::Var(_), x) | (x, v @ ast::Expr::Var(_)) => (v, x),
            _ => return,
        };
        if let (ast::Expr::Var(k), Some(n)) = (var, number(x))
            && self.keys.contains(k) {
                self.key_comparisons.push((k, n, self.loc(var)));
            }
    }

    /// Records `$3 > 100` (either way round), checked against the input with a data sample.
    fn check_field_comparison(&mut self, l: &'a AstExpr<'a>, r: &'a AstExpr<'a>) {
        let (field, n, at) = match (FieldRef::of(l), number(r), FieldRef::of(r), number(l)) {
            (Some(f), Some(n), _, _) => (f, n, l),
            (_, _, Some(f), Some(n)) => (f, n, r),
            _ => return,
        };
        self.field_comparisons.push((field, n, self.loc(at)));
    }

    /// Records `$3 > "100"` (either way round): a string constant makes it a string comparison
    /// ("99" > "100").
    fn check_string_comparison(&mut self, l: &'a AstExpr<'a>, r: &'a AstExpr<'a>) {
        let (field, s) = match (l, r) {
            (f @ ast::Expr::Unop(ast::Unop::Column, _), ast::Expr::StrLit(s))
            | (ast::Expr::StrLit(s), f @ ast::Expr::Unop(ast::Unop::Column, _)) => (f, *s),
            _ => return,
        };
        if crate::runtime::compare::looks_numeric(s).is_some() {
            let what = FieldRef::of(field)
                .map_or("the field".to_string(), |f| format!("`{}`", f.describe()));
            self.string_comparisons.push((what, s, self.loc(field)));
        }
    }

    /// Records `a[i++] = i` and `a[p(1)] = p(2)`: zawk evaluates the subscript before the
    /// right-hand side, gawk after, so the two disagree when the sides affect each other.
    fn check_assign_order(&mut self, lhs: &'a AstExpr<'a>, rhs: &'a AstExpr<'a>) {
        if let ast::Expr::Index(_, ix) = lhs {
            let l = Effects::of(ix, &self.user_funcs);
            let r = Effects::of(rhs, &self.user_funcs);
            if l.conflicts(&r) {
                self.order_dependent.push(self.loc(lhs));
            }
        }
    }

    /// Walks an assignment target.
    fn lvalue(&mut self, e: &'a AstExpr<'a>, kind: SetKind) {
        self.check_read_only(e);
        match e {
            ast::Expr::Var(name) => self.set(name, kind, self.loc(e)),
            ast::Expr::Index(arr, ix) => {
                self.array(arr);
                match arr {
                    ast::Expr::Var(name) => {
                        self.set(name, SetKind::Indirect, self.loc(arr));
                        if !self.is_local(name) {
                            self.element_sets.push(Set {
                                name,
                                region: self.region,
                                kind,
                                loc: self.loc(e),
                            });
                        }
                    }
                    // `a[i][j] = v` (an error reported by the type checker) sets `a`.
                    ast::Expr::Index(..) => self.lvalue(arr, SetKind::Indirect),
                    _ => self.expr(arr),
                }
                self.expr(ix);
            }
            _ => self.expr(e),
        }
    }

    fn expr(&mut self, e: &'a AstExpr<'a>) {
        use ast::Expr::*;
        let func = |f: &Either<&str, Function>| match f {
            Either::Left(name) => Function::try_from(*name).ok(),
            Either::Right(f) => Some(*f),
        };
        if let Some((lit, pat, reason)) = ast::invalid_string_regex(e, func) {
            self.string_regexes.push((pat, reason, self.loc(lit)));
        }
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | ReadStdin | Cond(_) => {}
            Var(name) => {
                if self.region == Region::Begin && matches!(*name, "NF" | "FILENAME") {
                    self.begin_record_reads
                        .push((format!("`{}`", name), self.loc(e)));
                }
                self.used(name, self.loc(e))
            }
            Unop(op, x) => {
                if self.region == Region::Begin && matches!(op, ast::Unop::Column) {
                    let what = match (FieldRef::of(e), x) {
                        (Some(f), _) => format!("`{}`", f.describe()),
                        (None, Unop(ast::Unop::Neg, ILit(n))) => format!("a field (`$(-{})`)", n),
                        _ => "a computed field (`$(...)`)".to_string(),
                    };
                    self.begin_record_reads.push((what, self.loc(e)));
                }
                // `$"age"` is `$0`: the string converts to a number, it does not name a column.
                if matches!(op, ast::Unop::Column)
                    && let Some(s) = literal(x)
                        && !std::str::from_utf8(&s).is_ok_and(looks_numeric) {
                            self.string_fields.push((s, self.loc(e)));
                        }
                if self.region != Region::Begin
                    && let (ast::Unop::Column, ILit(n)) = (op, x) {
                        self.fields.push((*n, self.loc(e)));
                    }
                self.expr(x)
            }
            Binop(ast::Binop::LT | ast::Binop::GT | ast::Binop::LTE | ast::Binop::GTE, l, r) => {
                self.check_key_comparison(l, r);
                self.check_field_comparison(l, r);
                self.check_string_comparison(l, r);
                self.expr(l);
                self.expr(r);
            }
            // `$NF-1` is the last field minus 1; `$(NF-1)` is the field before the last.
            Binop(
                op @ (ast::Binop::Minus | ast::Binop::Plus),
                l @ Unop(ast::Unop::Column, Var("NF")),
                r,
            ) => {
                if let ILit(n @ 1..) = r {
                    let op = if matches!(op, ast::Binop::Minus) {
                        "-"
                    } else {
                        "+"
                    };
                    self.field_offsets.push((op, *n, self.loc(l)));
                }
                self.expr(l);
                self.expr(r);
            }
            Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) => {
                self.expr(l);
                self.expr(r);
            }
            Index(arr, ix) => {
                if matches!(arr, Var("FI")) {
                    let name = match ix {
                        StrLit(k) => Some(*k),
                        _ => None,
                    };
                    self.header_columns.push((name, self.loc(e)));
                }
                if let Var(name) = arr {
                    self.element_reads.push((name, self.region));
                }
                self.array(arr);
                self.expr(arr);
                self.expr(ix);
            }
            Assign(lhs, rhs) => {
                if let (Var("FS"), StrLit(fs), Region::Begin) = (lhs, rhs, self.region) {
                    self.fs_literals.push(fs);
                }
                if matches!(lhs, Var("FS")) && self.region == Region::Main {
                    self.main_fs_sets.push(self.loc(lhs));
                }
                // `x = x + e` adds up like `x += e`.
                let sum = match (lhs, rhs) {
                    (Var(x), Binop(ast::Binop::Plus | ast::Binop::Minus, l, r)) => {
                        matches!(l, Var(y) if y == x) || matches!(r, Var(y) if y == x)
                    }
                    // `a[k] = a[k] + e`
                    (Index(Var(x), _), Binop(ast::Binop::Plus | ast::Binop::Minus, l, r)) => {
                        matches!(l, Index(Var(y), _) if y == x)
                            || matches!(r, Index(Var(y), _) if y == x)
                    }
                    _ => false,
                };
                self.check_assign_order(lhs, rhs);
                self.lvalue(lhs, if sum { SetKind::Sum } else { SetKind::Other });
                self.expr(rhs);
            }
            AssignOp(lhs, op, rhs) => {
                let kind = match op {
                    ast::Binop::Plus | ast::Binop::Minus => SetKind::Sum,
                    _ => SetKind::Other,
                };
                self.check_assign_order(lhs, rhs);
                self.lvalue(lhs, kind);
                self.expr(rhs);
            }
            Inc { x, .. } => self.lvalue(x, SetKind::Sum),
            ITE(c, t, f) => {
                self.condition(c);
                self.expr(t);
                self.expr(f);
            }
            Getline { into, from, .. } => {
                if matches!(self.region, Region::Begin | Region::Function) {
                    self.getline = true;
                }
                if into.is_none() && self.region != Region::Begin {
                    self.record_changed = true;
                }
                if let Some(into) = into {
                    if let Var(name) = into {
                        self.getline_vars.push(name);
                    }
                    self.lvalue(into, SetKind::Other);
                }
                if let Some(from) = from {
                    self.expr(from);
                }
            }
            Call(f, args) => {
                self.check_decimal_args(f, args, self.loc(e));
                let read_only = match f {
                    Either::Left(name) => {
                        self.calls.push((name, args.len(), self.loc(e)));
                        if *name == "sprintf"
                            && let Some(fmt) = args.first().and_then(|f| literal(f)) {
                                self.formats.push((fmt, args.len() - 1, self.loc(e)));
                            }
                        // A regex literal as the second argument is a compile error (E300).
                        if *name == "index" && matches!(args.first(), Some(PatLit(_))) {
                            self.index_regexes.push(self.loc(e));
                        }
                        READ_ONLY_FUNCTIONS.contains(name)
                    }
                    Either::Right(Function::Delete | Function::Clear) => {
                        if let Some(arr) = args.first() {
                            self.check_read_only(arr);
                        }
                        false
                    }
                    Either::Right(Function::Contains) => {
                        if let Some(arr) = args.first() {
                            self.array(arr);
                        }
                        true
                    }
                    Either::Right(_) => true,
                };
                for arg in args.iter() {
                    if let (Either::Left(f), Var(name)) = (f, arg)
                        && self.user_funcs.contains(f) {
                            self.element_reads.push((name, self.region));
                        }
                    match arg {
                        Var(name) if !read_only => self.set(name, SetKind::Indirect, self.loc(arg)),
                        _ => self.expr(arg),
                    }
                }
            }
        }
    }
}

impl<'a, 'p> Facts<'a, 'p> {
    /// `dec_add(t, $3 * 1.06)`: the product is a float, computed in binary floating point
    /// before `dec_add` sees it, so the decimal result is not exact.
    fn check_decimal_args(
        &mut self,
        f: &Either<&'a str, Function>,
        args: &[&AstExpr<'a>],
        loc: Option<Loc>,
    ) {
        let name = match f {
            Either::Right(Function::Decimal(op)) => op.func_name(),
            Either::Right(Function::DecCmp) => "dec_cmp",
            Either::Left(name) => match crate::builtins::FUNCTIONS.get(name) {
                Some(Function::Decimal(op)) => op.func_name(),
                Some(Function::DecCmp) => "dec_cmp",
                _ => return,
            },
            _ => return,
        };
        for arg in args {
            let arith = matches!(
                arg,
                ast::Expr::Binop(
                    ast::Binop::Plus
                        | ast::Binop::Minus
                        | ast::Binop::Mult
                        | ast::Binop::Div
                        | ast::Binop::Mod
                        | ast::Binop::Pow,
                    _,
                    _
                )
            );
            if arith {
                self.decimal_float_args.push((name, loc));
            }
        }
    }
}

/// The text of a number literal (with its sign).
fn number(e: &AstExpr) -> Option<String> {
    match e {
        ast::Expr::ILit(n) => Some(n.to_string()),
        ast::Expr::FLit(f) => Some(f.to_string()),
        ast::Expr::Unop(ast::Unop::Neg, x) => number(x).map(|n| format!("-{}", n)),
        ast::Expr::Unop(ast::Unop::Pos, x) => number(x),
        _ => None,
    }
}

/// Whether `e` reads a field (`$0`, `$1 "\n"`, `f"{$1}"`).
fn has_field(e: &AstExpr) -> bool {
    use ast::Expr::*;
    match e {
        Unop(ast::Unop::Column, _) => true,
        Unop(_, x) => has_field(x),
        Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) | Index(l, r) => {
            has_field(l) || has_field(r)
        }
        ITE(c, t, f) => has_field(c) || has_field(t) || has_field(f),
        Call(_, args) => args.iter().any(|a| has_field(a)),
        _ => false,
    }
}

/// The text of a string literal, or of a concatenation of string literals (`"%s" "\n"`).
fn literal(e: &AstExpr) -> Option<Vec<u8>> {
    match e {
        ast::Expr::StrLit(s) => Some(s.to_vec()),
        ast::Expr::Binop(ast::Binop::Concat, l, r) => {
            let mut s = literal(l)?;
            s.extend(literal(r)?);
            Some(s)
        }
        _ => None,
    }
}

/// The number of arguments a `printf` format uses: the sequential ones, or the highest positional
/// reference (`%2$s`, `*2$`), whichever is larger.
fn format_arguments(fmt: &[u8]) -> usize {
    // A positional reference `N$` at `fmt[*i..]`: its number, with `i` moved past it.
    let position = |i: &mut usize| -> Option<usize> {
        let start = *i;
        let mut j = start;
        while j < fmt.len() && fmt[j].is_ascii_digit() {
            j += 1;
        }
        if j == start || fmt.get(j) != Some(&b'$') {
            return None;
        }
        let n = std::str::from_utf8(&fmt[start..j])
            .ok()?
            .parse()
            .ok()
            .filter(|n| *n > 0)?;
        *i = j + 1;
        Some(n)
    };
    let mut n = 0;
    let mut max_position = 0;
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if fmt.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        let value = position(&mut i);
        while i < fmt.len() && b"-+ #0'_".contains(&fmt[i]) {
            i += 1;
        }
        for part in 0..2 {
            if part == 1 {
                if fmt.get(i) != Some(&b'.') {
                    break;
                }
                i += 1;
            }
            if fmt.get(i) == Some(&b'*') {
                i += 1;
                match position(&mut i) {
                    Some(p) => max_position = max_position.max(p),
                    None => n += 1,
                }
            } else {
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
        while i < fmt.len() && b"hlLqjzt".contains(&fmt[i]) {
            i += 1;
        }
        if i < fmt.len() {
            match value {
                Some(p) => max_position = max_position.max(p),
                None => n += 1,
            }
            i += 1;
        }
    }
    n.max(max_position)
}

fn plural(n: usize, noun: &str) -> String {
    format!("{} {}{}", n, noun, if n == 1 { "" } else { "s" })
}

fn warning(code: &'static str, message: String, loc: Option<Loc>) -> Diagnostic {
    Diagnostic {
        code,
        severity: Severity::Warning,
        message: crate::ast::source_names(&message),
        loc,
    }
}

/// Checks the syntax tree for mistakes that a run accepts silently.
fn check_ast<'a>(text: &str, prog: &ast::Prog<'a, 'a, &'a str>, opts: &Options) -> Vec<Diagnostic> {
    let mut facts = Facts {
        locs: &prog.locs,
        region: Region::Begin,
        locals: &[],
        uses: Vec::new(),
        sets: Vec::new(),
        arrays: Vec::new(),
        calls: Vec::new(),
        formats: Vec::new(),
        dynamic_formats: Vec::new(),
        field_offsets: Vec::new(),
        decimal_float_args: Vec::new(),
        string_comparisons: Vec::new(),
        main_fs_sets: Vec::new(),
        string_conds: Vec::new(),
        getline_vars: Vec::new(),
        read_only: Vec::new(),
        header_columns: Vec::new(),
        fields: Vec::new(),
        field_comparisons: Vec::new(),
        record_changed: false,
        fs_literals: Vec::new(),
        fs_sets: 0,
        begin_record_reads: Vec::new(),
        getline: false,
        keys: Vec::new(),
        key_comparisons: Vec::new(),
        numeric_redirects: Vec::new(),
        cond_assigns: Vec::new(),
        index_regexes: Vec::new(),
        string_regexes: Vec::new(),
        string_range_bounds: Vec::new(),
        string_fields: Vec::new(),
        user_funcs: prog.decs.iter().map(|d| d.name).collect(),
        order_dependent: Vec::new(),
        element_sets: Vec::new(),
        element_reads: Vec::new(),
    };
    prog.begin.iter().for_each(|s| facts.stmt(s));
    facts.region = Region::Main;
    for (pat, body) in prog.pats.iter() {
        match pat {
            ast::Pattern::Null => {}
            ast::Pattern::Bool(e) => facts.condition(e),
            ast::Pattern::Comma(l, r) => {
                facts.condition(l);
                facts.condition(r);
            }
        }
        if let Some(body) = body {
            facts.stmt(body);
        }
    }
    let worker_blocks = prog
        .beginfile
        .iter()
        .chain(prog.endfile.iter())
        .chain(prog.prepare.iter());
    // Elements set once per worker (`PREPARE { m[PID] = max }`) are the documented way to
    // collect per-worker values, not a W303.
    let main_element_sets = facts.element_sets.len();
    worker_blocks.for_each(|s| facts.stmt(s));
    facts.element_sets.truncate(main_element_sets);
    facts.region = Region::End;
    prog.end.iter().for_each(|s| facts.stmt(s));
    facts.region = Region::Function;
    for dec in prog.decs.iter() {
        facts.locals = &dec.args;
        facts.stmt(dec.body);
    }

    let mut diags: Vec<Diagnostic> = facts
        .read_only
        .iter()
        .map(|(msg, loc)| Diagnostic {
            code: "E305",
            severity: Severity::Error,
            message: msg.clone(),
            loc: *loc,
        })
        .collect();

    // Without a header `FI` is empty, so every `$FI["col"]` is `""`. Reported once, at the first use.
    if !opts.header && !facts.header_columns.is_empty() {
        let first = facts
            .header_columns
            .iter()
            .filter_map(|(_, l)| *l)
            .min_by_key(|l| (l.line, l.col));
        diags.push(warning(
            "W105",
            "`FI` is only filled from the header line with `-H` (or `-i jsonl|parquet|avro|markdown`): without it `FI` is empty and `$FI[\"col\"]` is always \"\"; run with `-H` (and pass `-H` to lint)".to_string(),
            first,
        ));
    }

    // BEGIN runs before any input is read: fields are empty, `NF` is 0 and `FILENAME` is "".
    // Each kind of read is reported once; a `getline` in BEGIN (or a function) may read a record.
    if !facts.getline {
        let mut reported: Vec<&str> = Vec::new();
        for (what, loc) in &facts.begin_record_reads {
            if reported.contains(&what.as_str()) {
                continue;
            }
            reported.push(what);
            let value = match what.as_str() {
                "`NF`" => "it is always 0",
                _ => "it is always empty",
            };
            diags.push(warning(
                "W218",
                format!(
                    "{} is read in BEGIN, before any input is read: {}; move it to a main rule (e.g. `NR == 1 {{ ... }}`) or read a record with `getline` first",
                    what, value
                ),
                *loc,
            ));
        }
    }

    // Array keys are strings: gawk compares `k > 9` as strings, zawk as numbers.
    for (key, n, loc) in &facts.key_comparisons {
        diags.push(warning(
            "W108",
            format!(
                "`{}` is an array key, which is a string: gawk compares it with {} as strings (\"10\" < \"9\"), zawk as numbers; write `{}+0` to compare as numbers in every awk",
                key, n, key
            ),
            *loc,
        ));
    }

    // `print $1 > 10` writes to the file `10`: `>` in print redirects, it does not compare.
    for (redirect, name, loc) in &facts.numeric_redirects {
        let file = match name {
            Some(n) => format!("the file `{}`", n),
            None => "a file named by a number".to_string(),
        };
        diags.push(warning(
            "W114",
            format!(
                "`{}` after print/printf redirects the output to {}, it does not compare; to print a comparison put it in parentheses, e.g. `print ($1 > 10)`",
                redirect, file
            ),
            *loc,
        ));
    }

    // `if (x = 1)` assigns and is always true: almost always a typo for `==`.
    for (name, loc) in &facts.cond_assigns {
        let what = match name {
            Some(n) => format!("`{}` is assigned", n),
            None => "an assignment is".to_string(),
        };
        diags.push(warning(
            "W115",
            format!(
                "{} in a condition: the condition is the assigned value, not a comparison; use `==` to compare, or if the assignment is meant, compare its value explicitly, e.g. `if ((x = f()) != \"\")`",
                what
            ),
            *loc,
        ));
    }

    // `for (i in "a".."c")`: range bounds are integers, a string is 0 unless it starts with a number.
    for (what, loc) in &facts.string_range_bounds {
        diags.push(warning(
            "W208",
            format!(
                "a bound of `for (i in start..end)` is {}: bounds are integers and a string becomes 0 unless it starts with a number, so the loop may run no iteration; use numbers, e.g. `for (i in 1..n)`",
                what
            ),
            *loc,
        ));
    }

    // `$"age"` converts the string to a number: `$0` (or `$3` for "3abc"), never the column `age`.
    for (s, loc) in &facts.string_fields {
        let lit = String::from_utf8_lossy(s);
        let lit = lit.escape_debug();
        let field = if starts_numeric(s) {
            "the field numbered by its leading digits"
        } else {
            "`$0`, the whole record"
        };
        diags.push(warning(
            "W121",
            format!(
                "`$\"{lit}\"`: a string after `$` is converted to a number, so this is {field}, not a column named \"{lit}\"; to read a column by its header name use `$FI[\"{lit}\"]` (with `-H`, or `-i jsonl|parquet|avro|markdown`), or write the field number"
            ),
            *loc,
        ));
    }

    // `a[i++] = i`: zawk evaluates the subscript first, gawk the right-hand side.
    for loc in &facts.order_dependent {
        diags.push(warning(
            "W122",
            "the subscript and the right-hand side of this assignment affect each other (e.g. `a[i++] = i`, or both call functions): zawk evaluates the subscript first, gawk the right-hand side, so the results differ; compute the subscript or the value in a separate statement first, e.g. `k = i++; a[k] = i`".to_string(),
            *loc,
        ));
    }

    // `$NF-1` is `($NF)-1`: `$` binds tighter than `-`.
    for (op, n, loc) in &facts.field_offsets {
        diags.push(warning(
            "W116",
            format!(
                "`$NF{op}{n}` is the last field {} {n}, not a field: `$` binds tighter than `{op}`; write `$(NF{op}{n})` for the field, or `($NF){op}{n}` if the arithmetic is meant",
                if *op == "-" { "minus" } else { "plus" }
            ),
            *loc,
        ));
    }

    // `$3 > "100"` compares as strings: "99" > "100".
    for (field, s, loc) in &facts.string_comparisons {
        let s = String::from_utf8_lossy(s);
        diags.push(warning(
            "W117",
            format!(
                "{} is compared with the string \"{}\", so it is compared as a string (\"99\" > \"100\"); drop the quotes (`{}`) to compare as numbers",
                field, s.escape_debug(), s.trim()
            ),
            *loc,
        ));
    }

    // `dec_add(t, $3 * 1.06)`: the arithmetic is done in binary floating point first.
    for (name, loc) in &facts.decimal_float_args {
        diags.push(warning(
            "W123",
            format!(
                "argument of `{name}` is computed with floating-point arithmetic before `{name}` sees it (`0.1 + 0.2` is 0.30000000000000004), so the result is not exact decimal; compute it with the dec_* functions instead, e.g. `dec_add(t, dec_mul($3, \"1.06\"))`"
            ),
            *loc,
        ));
    }

    // `printf $0`: a `%` in the data is read as a conversion.
    for loc in &facts.dynamic_formats {
        diags.push(warning(
            "W118",
            "printf with a format that reads a field and no arguments: a `%` in the data is read as a conversion (`100%d` prints `1000`); write `printf \"%s\", ...` instead".to_string(),
            *loc,
        ));
    }

    // `{ FS = "," }`: the current record is already split with the old separator.
    if let Some(loc) = facts.main_fs_sets.first() {
        diags.push(warning(
            "W119",
            "`FS` is assigned in a main rule: the current record is already split, so the new separator only applies from the next record (the first record is split wrongly); set it in BEGIN or with `-F`".to_string(),
            *loc,
        ));
    }

    // `if ("0")`: a string constant as a condition is almost always a mistake.
    for (s, loc) in &facts.string_conds {
        let message = if s.is_empty() {
            "the condition is the empty string: it is always false; compare a value instead"
                .to_string()
        } else {
            let lit = String::from_utf8_lossy(s);
            if crate::runtime::compare::str_is_false(s, true) {
                format!(
                    "the condition is the string constant \"{}\": zawk treats it as false (it looks like 0), gawk as true (a non-empty string); compare a value instead, e.g. `x == \"{}\"`",
                    lit.escape_debug(),
                    lit.escape_debug()
                )
            } else {
                format!(
                    "the condition is the string constant \"{}\": it is always true; compare a value instead, e.g. `$1 == \"{}\"`",
                    lit.escape_debug(),
                    lit.escape_debug()
                )
            }
        };
        diags.push(warning("W120", message, *loc));
    }

    // Variables that are read but never set anywhere: almost always a typo.
    let mut reported: Vec<&str> = Vec::new();
    for u in &facts.uses {
        let special = crate::builtins::VARIABLES.contains_key(u.name)
            || u.name == "SUBSEP"
            || crate::ast::is_uninit_var(u.name)
            || IGNORED_GAWK_VARIABLES.iter().any(|(n, _)| *n == u.name);
        if special
            || reported.contains(&u.name)
            || facts.sets.iter().any(|s| s.name == u.name)
            || opts.assigned.iter().any(|v| v == u.name)
        {
            continue;
        }
        reported.push(u.name);
        let first = facts
            .uses
            .iter()
            .filter(|v| v.name == u.name)
            .filter_map(|v| v.loc)
            .min_by_key(|l| (l.line, l.col));
        diags.push(warning(
            "W201",
            format!(
                "variable `{}` is used but never set: it is always empty (if it comes from the command line, pass `-v {}=...` to lint or declare it with `# @var {}`)",
                u.name, u.name, u.name
            ),
            first.or(u.loc),
        ));
    }
    let unset = reported;

    // Variables that are set but never read: the other side of a typo (`totl += $1` ...
    // `print total`). Arrays, `for (k in a)` keys, variables filled by a function and
    // `getline var` (often only to skip a line) are left out.
    let mut reported: Vec<&str> = Vec::new();
    for s in &facts.sets {
        let special = crate::builtins::VARIABLES.contains_key(s.name)
            || s.name == "SUBSEP"
            || IGNORED_GAWK_VARIABLES.iter().any(|(n, _)| *n == s.name);
        if special
            || s.kind == SetKind::Indirect
            || reported.contains(&s.name)
            || facts.uses.iter().any(|u| u.name == s.name)
            || facts.arrays.contains(&s.name)
            || facts.getline_vars.contains(&s.name)
            || prog.decs.iter().any(|d| d.name == s.name)
            || facts
                .sets
                .iter()
                .any(|t| t.name == s.name && t.kind == SetKind::Indirect)
        {
            continue;
        }
        reported.push(s.name);
        let name = crate::ast::source_name(s.name);
        let typo = unset
            .iter()
            .map(|u| (edit_distance(name, u), *u))
            .filter(|(d, _)| *d <= 2 && *d < name.chars().count())
            .min_by_key(|(d, _)| *d)
            .map_or(String::new(), |(_, u)| {
                format!("; `{}` is read but never set, is one of them a typo?", u)
            });
        let what = if crate::ast::is_local_name(s.name) {
            "local variable"
        } else {
            "variable"
        };
        diags.push(warning(
            "W206",
            format!("{} `{}` is set but never read{}", what, name, typo),
            s.loc,
        ));
    }

    // User functions that are never called (unless the program is only functions, a library).
    let rules = !prog.begin.is_empty() || !prog.pats.is_empty() || !prog.end.is_empty();
    for dec in prog.decs.iter().filter(|_| rules) {
        if !facts.calls.iter().any(|(n, _, _)| *n == dec.name) {
            let loc = lexer::Tokenizer::new(text)
                .map_while(Result::ok)
                .find(|(_, tok, _)| matches!(tok, Tok::FunDec(n) if *n == dec.name))
                .map(|(l, _, _)| l);
            diags.push(warning(
                "W207",
                format!("function `{}` is defined but never called", dec.name),
                loc,
            ));
        }
    }

    // printf formats that use more arguments than given: the missing ones print as empty.
    for (fmt, given, loc) in &facts.formats {
        let wanted = format_arguments(fmt);
        if wanted > *given {
            diags.push(warning(
                "W202",
                format!(
                    "format \"{}\" uses {}, got {}: the missing ones are empty",
                    String::from_utf8_lossy(fmt).escape_debug(),
                    plural(wanted, "argument"),
                    given
                ),
                *loc,
            ));
        } else if wanted < *given {
            diags.push(warning(
                "W203",
                format!(
                    "format \"{}\" uses {}, got {}: the extra ones are ignored",
                    String::from_utf8_lossy(fmt).escape_debug(),
                    plural(wanted, "argument"),
                    given
                ),
                *loc,
            ));
        }
    }

    // Calls of user functions with more arguments than parameters.
    for (name, given, loc) in &facts.calls {
        if let Some(dec) = prog.decs.iter().find(|d| d.name == *name)
            && *given > dec.params {
                diags.push(warning(
                    "W204",
                    format!(
                        "function `{}` is called with {} but declares {}: the extra arguments are ignored",
                        name,
                        plural(*given, "argument"),
                        plural(dec.params, "parameter")
                    ),
                    *loc,
                ));
            }
    }

    // `index(/re/, s)`: a regex literal as a value is `$0 ~ /re/`, so this searches in "0" or "1".
    for loc in &facts.index_regexes {
        diags.push(warning(
            "W205",
            "`index` takes a string, not a regex: `/re/` here means `$0 ~ /re/` (0 or 1); use `index(s, \"text\")`, or `match(s, /re/)` to find the position of a regex match".to_string(),
            *loc,
        ));
    }

    // `s ~ "["`: a string used as a regex is compiled when the run first uses it, and fails it.
    for (pat, reason, loc) in &facts.string_regexes {
        diags.push(Diagnostic {
            code: "E307",
            severity: Severity::Error,
            message: format!("invalid regex /{}/: {}", pat, reason),
            loc: *loc,
        });
    }

    if let Some(mode) = opts.parallel {
        diags.extend(check_parallel(&facts, prog, mode));
    }
    if let Some(data) = &opts.data {
        diags.extend(check_data(&facts, opts, data));
    }
    diags
}

/// How a run splits lines into fields.
enum Splitter {
    /// The default `FS = " "`: runs of blanks and newlines, ignoring leading and trailing ones.
    Blank,
    Char(char),
    Regex(regex::Regex),
}

impl Splitter {
    fn new(fs: &str) -> Option<Splitter> {
        let mut chars = fs.chars();
        Some(match (chars.next(), chars.next()) {
            (Some(' '), None) => Splitter::Blank,
            (Some(c), None) if c != '\\' => Splitter::Char(c),
            _ => Splitter::Regex(regex::Regex::new(fs).ok()?),
        })
    }

    fn split(&self, line: &str) -> Vec<String> {
        if line.is_empty() {
            return Vec::new();
        }
        match self {
            Splitter::Blank => line
                .split([' ', '\t', '\n'])
                .filter(|f| !f.is_empty())
                .map(String::from)
                .collect(),
            Splitter::Char(c) => line.split(*c).map(String::from).collect(),
            Splitter::Regex(re) => re.split(line).map(String::from).collect(),
        }
    }
}

/// Whether awk reads `s`, a field, as a number (a "strnum"), by the rules of the runtime.
fn looks_numeric(s: &str) -> bool {
    crate::runtime::compare::looks_numeric(s.as_bytes()).is_some()
}

/// Whether `s` starts with a number (after blanks and a sign), so it is not 0 as an integer.
fn starts_numeric(s: &[u8]) -> bool {
    let s = s.trim_ascii_start();
    let s = s
        .strip_prefix(b"-")
        .or_else(|| s.strip_prefix(b"+"))
        .unwrap_or(s);
    let s = s.strip_prefix(b".").unwrap_or(s);
    s.first().is_some_and(u8::is_ascii_digit)
}

/// The edit distance between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The column the author probably meant by `name`: the same name in another case, the same name
/// with a `:TYPE` suffix, or a name at most 2 edits away.
fn closest_column<'c>(name: &str, columns: &'c [String]) -> Option<&'c str> {
    let lower = name.to_lowercase();
    columns
        .iter()
        .filter_map(|c| {
            let base = c.rsplit_once(':').map_or(c.as_str(), |(b, _)| b);
            let d = if c.to_lowercase() == lower || base == name {
                0
            } else {
                edit_distance(&lower, &c.to_lowercase())
            };
            (d <= 2 && d < name.chars().count().max(2)).then_some((d, c.as_str()))
        })
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Checks the program against the first records of an input file: unknown column names, fields
/// beyond the fields of the records, a wrong field separator and number comparisons of fields
/// that are not numbers.
fn check_data(facts: &Facts, opts: &Options, data: &Data) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let file = &data.file;
    // The separator of the run, for text: `FS = "..."` in BEGIN, else `-F`, else " ".
    let (header, rows, fs) = match &data.sample {
        Sample::Fields { header, rows } => (header.clone(), rows.clone(), None),
        Sample::Lines(lines) => {
            let fs = match facts.fs_literals.as_slice() {
                [] if facts.fs_sets == 0 => {
                    opts.field_sep.clone().unwrap_or_else(|| " ".to_string())
                }
                [fs] if facts.fs_sets == 1 => String::from_utf8_lossy(fs).into_owned(),
                // FS changes in a way the sample cannot follow.
                _ => return diags,
            };
            let Some(splitter) = Splitter::new(&fs) else {
                return diags;
            };
            let mut records = lines.iter();
            let header = if opts.header {
                records
                    .next()
                    .map(|h| splitter.split(h.trim_start_matches('\u{feff}')))
            } else {
                None
            };
            (
                header,
                records.map(|l| splitter.split(l)).collect::<Vec<_>>(),
                Some((fs, lines)),
            )
        }
    };
    let max_nf = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let sampled = format!("the first {} of `{}`", plural(rows.len(), "record"), file);

    // Every record has one field, but the program reads `$2` or more: the separator is wrong.
    if let Some((fs, lines)) = &fs {
        let first_wide = facts.fields.iter().find(|(n, _)| *n >= 2);
        let unknown_header = header.as_ref().is_some_and(|h| h.len() == 1)
            && facts.header_columns.iter().any(|(k, _)| {
                k.is_some_and(|k| !header.as_ref().unwrap().iter().any(|c| c.as_bytes() == k))
            });
        let header_one = header.as_ref().is_none_or(|h| h.len() <= 1);
        if !rows.is_empty() && max_nf <= 1 && header_one && (first_wide.is_some() || unknown_header)
        {
            let shown = if fs == " " {
                "the default FS (blanks)".to_string()
            } else {
                format!("FS `{}`", fs.escape_default())
            };
            let candidates = [
                (',', "`-F,` or `-i csv`"),
                ('\t', "`-F'\\t'` or `-i tsv`"),
                (';', "`-F';'`"),
                ('|', "`-F'|'`"),
            ];
            let suggestion = candidates
                .iter()
                .filter(|(c, _)| !fs.contains(*c) && !(fs == " " && *c == '\t'))
                .filter(|(c, _)| {
                    lines
                        .iter()
                        .filter(|l| !l.is_empty())
                        .all(|l| l.contains(*c))
                })
                .max_by_key(|(c, _)| lines.iter().map(|l| l.matches(*c).count()).sum::<usize>())
                .map(|(c, opt)| {
                    format!(
                        "; the lines contain `{}`: did you mean {}?",
                        c.escape_default(),
                        opt
                    )
                })
                .unwrap_or_default();
            let what = if first_wide.is_some() {
                "fields after `$1`"
            } else {
                "columns of the header"
            };
            let loc = match first_wide {
                Some((_, l)) => *l,
                None => facts.header_columns.iter().find_map(|(_, l)| *l),
            };
            diags.push(warning(
                "W128",
                format!(
                    "the program reads {}, but with {} each of {} has at most one field{}",
                    what, shown, sampled, suggestion
                ),
                loc,
            ));
            return diags;
        }
    }

    // `$FI["nmae"]`: a column the header does not have; `FI` has no such key and the field is "".
    if let Some(header) = &header {
        let mut reported: Vec<&[u8]> = Vec::new();
        for (key, loc) in &facts.header_columns {
            let Some(key) = key else { continue };
            if header.iter().any(|c| c.as_bytes() == *key) || reported.contains(key) {
                continue;
            }
            reported.push(key);
            let name = String::from_utf8_lossy(key);
            let hint = match closest_column(&name, header) {
                Some(c) => format!("did you mean `{}`?", c),
                None => {
                    let mut cols: Vec<String> =
                        header.iter().take(20).map(|c| format!("`{}`", c)).collect();
                    if header.len() > 20 {
                        cols.push("...".to_string());
                    }
                    format!("the columns are {}", cols.join(", "))
                }
            };
            diags.push(Diagnostic {
                code: "W126",
                severity: Severity::Error,
                message: format!(
                    "unknown column `{}`: the header of `{}` has no such column, so `$FI[\"{}\"]` is always \"\"; {}",
                    name, file, name, hint
                ),
                loc: *loc,
            });
        }
    }

    // `$7` when no record has 7 fields.
    if !rows.is_empty() && !facts.record_changed {
        let mut reported: Vec<i64> = Vec::new();
        for (n, loc) in &facts.fields {
            if *n as usize <= max_nf || *n < 1 || reported.contains(n) {
                continue;
            }
            reported.push(*n);
            diags.push(warning(
                "W127",
                format!(
                    "`${}` is empty in {}: they have at most {}",
                    n,
                    sampled,
                    plural(max_nf, "field")
                ),
                *loc,
            ));
        }
    }

    // `$3 > 100` where `$3` is not always a number: those records are compared as strings.
    for (field, number, loc) in &facts.field_comparisons {
        let index = match field {
            FieldRef::Num(n) if *n >= 1 => *n as usize,
            FieldRef::Name(k) => match header
                .as_ref()
                .and_then(|h| h.iter().position(|c| c.as_bytes() == *k))
            {
                Some(i) => i + 1,
                None => continue,
            },
            _ => continue,
        };
        let bad: Vec<(usize, &str)> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.get(index - 1).map(|v| (i + 1, v.as_str())))
            .filter(|(_, v)| !v.trim().is_empty() && !looks_numeric(v))
            .collect();
        let Some((record, value)) = bad.first() else {
            continue;
        };
        let name = field.describe();
        let shown: String = value.chars().take(40).collect();
        let advice = if bad.len() == 1 && *record == 1 && header.is_none() {
            "; the first record looks like a header line: run with `-H` (and pass `-H` to lint) or skip it with `NR > 1`".to_string()
        } else {
            format!(
                "; to compare numbers use `{} + 0` (non-numbers are 0) or skip those records first",
                name
            )
        };
        diags.push(warning(
            "W129",
            format!(
                "`{}` is compared with the number {}, but it is not a number in {} of {} (e.g. record {}: \"{}\"): those records are compared as strings{}",
                name,
                number,
                plural(bad.len(), "record"),
                sampled,
                record,
                shown,
                advice
            ),
            *loc,
        ));
    }
    diags
}

/// Code whose meaning changes when the main loop runs in several workers (see
/// `info/parallelism.md`).
fn check_parallel<'a>(
    facts: &Facts<'a, '_>,
    prog: &ast::Prog<'a, 'a, &'a str>,
    mode: Parallel,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // Record counters count the records of one worker.
    let counters: &[&str] = match mode {
        Parallel::Record => &["NR", "FNR"],
        Parallel::File => &["NR"],
    };
    for counter in counters {
        if let Some(u) = facts
            .uses
            .iter()
            .find(|u| u.region == Region::Main && u.name == *counter)
        {
            diags.push(warning(
                "W301",
                format!(
                    "in parallel mode each worker counts its own records: `{}` is not the record number of the input (e.g. `{} == 1` matches once per worker)",
                    counter, counter
                ),
                u.loc,
            ));
        }
    }
    // Scalars set in the main loop and read in END are summed (numbers) or taken from one worker
    // (strings): only sums aggregate to the serial result.
    let mut reported: Vec<&str> = Vec::new();
    for s in &facts.sets {
        if s.region != Region::Main
            || s.kind != SetKind::Other
            || reported.contains(&s.name)
            || facts.arrays.contains(&s.name)
            || crate::builtins::VARIABLES.contains_key(s.name)
            || !facts
                .uses
                .iter()
                .any(|u| u.region == Region::End && u.name == s.name)
        {
            continue;
        }
        reported.push(s.name);
        diags.push(warning(
            "W302",
            format!(
                "in parallel mode END sees `{}` aggregated over the workers (numbers summed, strings from one worker), not its last value; collect per-worker values in PREPARE, e.g. `PREPARE {{ v[PID] = {} }}`",
                s.name, s.name
            ),
            s.loc,
        ));
    }
    // Arrays whose elements the main loop assigns (`a[k] = 1`, `last[k] = $2`) and END reads:
    // the workers' arrays are merged key by key, numbers summed and strings from one worker, so
    // a value seen by several workers is multiplied. Keys alone (`for (k in a)`, `length(a)`)
    // are the union and stay right.
    let mut reported: Vec<&str> = Vec::new();
    for s in &facts.element_sets {
        if s.region != Region::Main
            || s.kind != SetKind::Other
            || reported.contains(&s.name)
            || crate::builtins::VARIABLES.contains_key(s.name)
            || !facts
                .element_reads
                .iter()
                .any(|(n, r)| *r == Region::End && *n == s.name)
        {
            continue;
        }
        reported.push(s.name);
        diags.push(warning(
            "W303",
            format!(
                "in parallel mode END sees the elements of `{}` merged over the workers (numbers summed, strings from one worker), so a key assigned in several workers is counted several times; count with `{}[k]++` / `+=`, or collect per-worker values in PREPARE",
                s.name, s.name
            ),
            s.loc,
        ));
    }
    // A range pattern is on from the record matching its start to the one matching its end, but
    // each worker keeps its own range state and sees only its share of the records.
    if mode == Parallel::Record {
        for (pat, _) in prog.pats.iter() {
            if let ast::Pattern::Comma(l, _) = pat {
                diags.push(warning(
                    "W304",
                    "in parallel mode each worker keeps its own range state: a range pattern `start, end` misses the records whose range starts in another worker's chunk; run it serially (`-p0`), `--keep-order` does so".to_string(),
                    facts.locs.get(&ast::expr_key(*l)).copied(),
                ));
            }
        }
    }
    // State carried from one record to the next (`!seen[$1]++`, `$1 != prev`) is per worker.
    // Names already reported by W302/W303 (the first quoted name of their message) are left out.
    let reported_end: Vec<String> = diags
        .iter()
        .filter(|d| matches!(d.code, "W302" | "W303"))
        .filter_map(|d| d.message.split('`').nth(1).map(String::from))
        .collect();
    for (name, loc) in CrossRecord::find(prog) {
        if reported_end.iter().any(|n| n == name) {
            continue;
        }
        diags.push(warning(
            "W305",
            format!(
                "in parallel mode each worker keeps its own `{}`: its value carried over from earlier records only covers the worker's share of them, so what depends on it (e.g. `!seen[$1]++`, `$1 != prev`) is wrong at chunk boundaries or repeated once per worker; run it serially (`-p0`)",
                name
            ),
            loc,
        ));
    }
    diags
}

/// Finds state that the main loop carries from one record to the next and lets show: a global
/// read where it decides or makes output (a rule pattern, a `print` argument, the condition of
/// an `if` that prints) before the record assigns it afresh, such as `!seen[$1]++`,
/// `$1 != prev` or `print n` after `n++`. Each worker of a parallel run keeps its own copy, so
/// such a program prints once per worker (deduplication) or misses the records at chunk
/// boundaries. Accumulations (`n++`, `s += $1`, `m = m > $1 ? m : $1`) only feed themselves and
/// are left to W302/W303, which look at what END sees.
struct CrossRecord<'a, 'p> {
    locs: &'p ast::ExprLocs,
    /// Globals assigned somewhere in the main loop (scalars and arrays).
    written: Vec<&'a str>,
    /// Scalars assigned afresh earlier in the walk of a record (`x = $1`): reading them later
    /// sees this record's value.
    fresh: Vec<&'a str>,
    /// Arrays cleared or filled whole earlier in the walk (`split($0, a)`, `delete a`).
    fresh_arrays: Vec<&'a str>,
    /// Array elements with a literal key assigned earlier in the walk (`a["k"] = $1`).
    fresh_elements: Vec<(&'a str, String)>,
    found: Vec<(&'a str, Option<Loc>)>,
}

/// Whether `s` prints, so that what decides whether it runs shows in the output.
fn prints(s: &AstStmt) -> bool {
    use ast::Stmt::*;
    match s {
        Print(..) | Printf(..) => true,
        Block(ss) => ss.iter().any(|s| prints(s)),
        If(_, t, f) => prints(t) || f.is_some_and(prints),
        For(_, _, _, body) | DoWhile(_, body) | While(_, _, body) | ForEach(_, _, body) => {
            prints(body)
        }
        ForRange(_, _, _, _, body) => prints(body),
        Switch(_, cases) => cases.iter().any(|(_, body)| prints(body)),
        EndBlock(s) => prints(s),
        _ => false,
    }
}

/// The text of a literal array key, to match `a["k"] = 1` with a later `a["k"]`.
fn literal_key(e: &AstExpr) -> Option<String> {
    match e {
        ast::Expr::ILit(n) => Some(n.to_string()),
        ast::Expr::StrLit(s) => Some(String::from_utf8_lossy(s).into_owned()),
        _ => None,
    }
}

/// Whether `e` reads the variable (or array) `name`.
fn reads_var(e: &AstExpr, name: &str) -> bool {
    use ast::Expr::*;
    match e {
        Var(n) => *n == name,
        ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | ReadStdin | Cond(_) => false,
        Unop(_, x) | Inc { x, .. } => reads_var(x, name),
        Binop(_, l, r)
        | And(l, r)
        | Or(l, r)
        | Nullish(l, r)
        | Try(l, r)
        | Index(l, r)
        | Assign(l, r)
        | AssignOp(l, _, r) => reads_var(l, name) || reads_var(r, name),
        ITE(c, t, f) => reads_var(c, name) || reads_var(t, name) || reads_var(f, name),
        Getline { into, from, .. } => {
            into.is_some_and(|x| reads_var(x, name)) || from.is_some_and(|x| reads_var(x, name))
        }
        Call(_, args) => args.iter().any(|a| reads_var(a, name)),
    }
}

impl<'a, 'p> CrossRecord<'a, 'p> {
    fn find(prog: &'p ast::Prog<'a, 'a, &'a str>) -> Vec<(&'a str, Option<Loc>)> {
        let mut w = CrossRecord {
            locs: &prog.locs,
            written: Vec::new(),
            fresh: Vec::new(),
            fresh_arrays: Vec::new(),
            fresh_elements: Vec::new(),
            found: Vec::new(),
        };
        // First pass: what the main loop assigns.
        for (pat, body) in prog.pats.iter() {
            match pat {
                ast::Pattern::Null => {}
                ast::Pattern::Bool(e) => w.collect_expr(e),
                ast::Pattern::Comma(l, r) => {
                    w.collect_expr(l);
                    w.collect_expr(r);
                }
            }
            if let Some(body) = body {
                w.collect_stmt(body);
            }
        }
        w.written
            .retain(|n| !crate::builtins::VARIABLES.contains_key(n));
        if w.written.is_empty() {
            return Vec::new();
        }
        // Second pass: the reads that show, in the order a record runs the rules.
        for (pat, body) in prog.pats.iter() {
            let shows = body.is_none_or(|b| prints(b));
            match pat {
                ast::Pattern::Null => {}
                ast::Pattern::Bool(e) => w.expr(e, shows),
                ast::Pattern::Comma(l, r) => {
                    w.expr(l, shows);
                    w.expr(r, shows);
                }
            }
            if let Some(body) = body {
                w.stmt(body);
            }
        }
        let mut found = w.found;
        let mut seen = Vec::new();
        found.retain(|(n, _)| {
            let first = !seen.contains(n);
            seen.push(*n);
            first
        });
        found
    }

    fn written_name(&mut self, e: &'a AstExpr<'a>) {
        let target = match e {
            ast::Expr::Index(arr, _) => arr,
            _ => e,
        };
        if let ast::Expr::Var(name) = target
            && !self.written.contains(name) {
                self.written.push(name);
            }
    }

    fn collect_stmt(&mut self, s: &'a AstStmt<'a>) {
        use ast::Stmt::*;
        match s {
            StartCond(_) | EndCond(_) | LastCond(_) | Break | Continue | Next | NextFile => {}
            Expr(e) | Abort(e) => self.collect_expr(e),
            Block(ss) => ss.iter().for_each(|s| self.collect_stmt(s)),
            Print(args, out) => {
                args.iter().for_each(|e| self.collect_expr(e));
                if let Some((e, _)) = out {
                    self.collect_expr(e);
                }
            }
            Printf(fmt, args, out) => {
                self.collect_expr(fmt);
                args.iter().for_each(|e| self.collect_expr(e));
                if let Some((e, _)) = out {
                    self.collect_expr(e);
                }
            }
            If(c, t, f) => {
                self.collect_expr(c);
                self.collect_stmt(t);
                if let Some(f) = f {
                    self.collect_stmt(f);
                }
            }
            For(init, cond, update, body) => {
                if let Some(s) = init {
                    self.collect_stmt(s);
                }
                if let Some(e) = cond {
                    self.collect_expr(e);
                }
                if let Some(s) = update {
                    self.collect_stmt(s);
                }
                self.collect_stmt(body);
            }
            DoWhile(c, body) | While(_, c, body) => {
                self.collect_expr(c);
                self.collect_stmt(body);
            }
            ForEach(key, arr, body) => {
                if !self.written.contains(key) {
                    self.written.push(key);
                }
                self.collect_expr(arr);
                self.collect_stmt(body);
            }
            ForRange(v, start, end, _, body) => {
                if !self.written.contains(v) {
                    self.written.push(v);
                }
                self.collect_expr(start);
                self.collect_expr(end);
                self.collect_stmt(body);
            }
            Switch(e, cases) => {
                self.collect_expr(e);
                for (label, body) in cases.iter() {
                    if let Some(l) = label {
                        self.collect_expr(l);
                    }
                    self.collect_stmt(body);
                }
            }
            Return(e) => {
                if let Some(e) = e {
                    self.collect_expr(e);
                }
            }
            EndBlock(s) => self.collect_stmt(s),
        }
    }

    fn collect_expr(&mut self, e: &'a AstExpr<'a>) {
        use ast::Expr::*;
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | ReadStdin | Cond(_) | Var(_) => {}
            Unop(_, x) => self.collect_expr(x),
            Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) | Index(l, r) => {
                self.collect_expr(l);
                self.collect_expr(r);
            }
            Assign(l, r) | AssignOp(l, _, r) => {
                self.written_name(l);
                self.collect_expr(l);
                self.collect_expr(r);
            }
            Inc { x, .. } => {
                self.written_name(x);
                self.collect_expr(x);
            }
            ITE(c, t, f) => {
                self.collect_expr(c);
                self.collect_expr(t);
                self.collect_expr(f);
            }
            Getline { into, from, .. } => {
                if let Some(into) = into {
                    self.written_name(into);
                }
                if let Some(from) = from {
                    self.collect_expr(from);
                }
            }
            Call(f, args) => {
                let read_only = match f {
                    Either::Left(name) => READ_ONLY_FUNCTIONS.contains(name),
                    Either::Right(Function::Delete | Function::Clear) => false,
                    Either::Right(_) => true,
                };
                for arg in args.iter() {
                    match arg {
                        Var(_) if !read_only => self.written_name(arg),
                        _ => self.collect_expr(arg),
                    }
                }
            }
        }
    }

    /// A read of `name` (an element with key `key` when it is an array).
    fn read(&mut self, name: &'a str, key: Option<&AstExpr<'a>>, e: &AstExpr<'a>, shows: bool) {
        if !shows || !self.written.contains(&name) {
            return;
        }
        let fresh = match key {
            None => self.fresh.contains(&name) || self.fresh_arrays.contains(&name),
            Some(k) => {
                self.fresh_arrays.contains(&name)
                    || literal_key(k).is_some_and(|k| {
                        self.fresh_elements
                            .iter()
                            .any(|(n, x)| *n == name && *x == k)
                    })
            }
        };
        if !fresh {
            self.found
                .push((name, self.locs.get(&ast::expr_key(e)).copied()));
        }
    }

    /// An assignment to `lhs`; `afresh` when the new value does not depend on the old one.
    fn assigned(&mut self, lhs: &'a AstExpr<'a>, afresh: bool) {
        match lhs {
            ast::Expr::Var(name) if afresh => self.fresh.push(name),
            ast::Expr::Index(ast::Expr::Var(name), k) if afresh => {
                if let Some(k) = literal_key(k) {
                    self.fresh_elements.push((name, k));
                }
            }
            _ => {}
        }
    }

    fn stmt(&mut self, s: &'a AstStmt<'a>) {
        use ast::Stmt::*;
        match s {
            StartCond(_) | EndCond(_) | LastCond(_) | Break | Continue | Next | NextFile => {}
            // The value of a statement-level `n++` or `s += $1` is not used.
            Expr(e) => match e {
                ast::Expr::Inc { x, .. } => self.lvalue(x, false),
                ast::Expr::AssignOp(l, _, r) => {
                    self.expr(r, false);
                    self.lvalue(l, false);
                }
                _ => self.expr(e, false),
            },
            Abort(e) => self.expr(e, false),
            Block(ss) => ss.iter().for_each(|s| self.stmt(s)),
            Print(args, out) => {
                args.iter().for_each(|e| self.expr(e, true));
                if let Some((e, _)) = out {
                    self.expr(e, true);
                }
            }
            Printf(fmt, args, out) => {
                self.expr(fmt, true);
                args.iter().for_each(|e| self.expr(e, true));
                if let Some((e, _)) = out {
                    self.expr(e, true);
                }
            }
            If(c, t, f) => {
                let shows = prints(t) || f.is_some_and(prints);
                self.expr(c, shows);
                self.stmt(t);
                if let Some(f) = f {
                    self.stmt(f);
                }
            }
            For(init, cond, update, body) => {
                if let Some(s) = init {
                    self.stmt(s);
                }
                if let Some(e) = cond {
                    self.expr(e, false);
                }
                if let Some(s) = update {
                    self.stmt(s);
                }
                self.stmt(body);
            }
            DoWhile(c, body) | While(_, c, body) => {
                self.expr(c, false);
                self.stmt(body);
            }
            ForEach(key, arr, body) => {
                self.fresh.push(key);
                self.expr(arr, false);
                self.stmt(body);
            }
            ForRange(v, start, end, _, body) => {
                self.fresh.push(v);
                self.expr(start, false);
                self.expr(end, false);
                self.stmt(body);
            }
            Switch(e, cases) => {
                let shows = cases.iter().any(|(_, body)| prints(body));
                self.expr(e, shows);
                for (label, body) in cases.iter() {
                    if let Some(l) = label {
                        self.expr(l, false);
                    }
                    self.stmt(body);
                }
            }
            Return(e) => {
                if let Some(e) = e {
                    self.expr(e, false);
                }
            }
            EndBlock(s) => self.stmt(s),
        }
    }

    /// Walks the subscript of an assigned element (its old value is read only by updates).
    fn lvalue(&mut self, lhs: &'a AstExpr<'a>, afresh: bool) {
        if let ast::Expr::Index(_, k) = lhs {
            self.expr(k, false);
        }
        self.assigned(lhs, afresh);
    }

    fn expr(&mut self, e: &'a AstExpr<'a>, shows: bool) {
        use ast::Expr::*;
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | ReadStdin | Cond(_) => {}
            Var(name) => self.read(name, None, e, shows),
            Unop(_, x) => self.expr(x, shows),
            Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) => {
                self.expr(l, shows);
                self.expr(r, shows);
            }
            Index(arr, k) => {
                match arr {
                    Var(name) => self.read(name, Some(k), e, shows),
                    _ => self.expr(arr, shows),
                }
                self.expr(k, shows);
            }
            Assign(lhs, rhs) => {
                // `m = m > $1 ? m : $1` only feeds itself.
                let target = match lhs {
                    Index(Var(name), _) | Var(name) => Some(*name),
                    _ => None,
                };
                let update = target.is_some_and(|n| reads_var(rhs, n));
                self.expr(rhs, shows && !update);
                if shows {
                    // `print (x = x + 1)` shows the old value.
                    if update {
                        self.expr(lhs, true);
                    }
                }
                self.lvalue(lhs, !update);
            }
            AssignOp(lhs, _, rhs) => {
                self.expr(rhs, shows);
                if shows {
                    self.expr(lhs, true);
                }
                self.lvalue(lhs, false);
            }
            // `!seen[$1]++`: the old value decides.
            Inc { x, .. } => {
                if shows {
                    self.expr(x, true);
                }
                self.lvalue(x, false);
            }
            ITE(c, t, f) => {
                self.expr(c, shows);
                self.expr(t, shows);
                self.expr(f, shows);
            }
            Getline { into, from, .. } => {
                if let Some(from) = from {
                    self.expr(from, shows);
                }
                if let Some(into) = into {
                    self.lvalue(into, true);
                }
            }
            Call(f, args) => {
                // `delete a[k]` removes one element; `delete a` and `split(s, a)` start afresh.
                let fills = match f {
                    Either::Left(name) => !READ_ONLY_FUNCTIONS.contains(name),
                    Either::Right(Function::Clear) => true,
                    Either::Right(Function::Delete) => {
                        args.iter().skip(1).for_each(|k| self.expr(k, false));
                        return;
                    }
                    Either::Right(_) => false,
                };
                for arg in args.iter() {
                    match arg {
                        Var(name) if fills => {
                            self.fresh_arrays.push(name);
                            self.fresh.push(name);
                        }
                        _ => self.expr(arg, shows),
                    }
                }
            }
        }
    }
}

/// gawk special variables that zawk treats as ordinary variables.
const IGNORED_GAWK_VARIABLES: &[(&str, &str)] = &[
    (
        "FIELDWIDTHS",
        "fixed-width field splitting is not supported; use substr() or FS",
    ),
    (
        "FPAT",
        "field splitting by content is not supported; use FS or match()/split()",
    ),
    ("RT", "the text matched by RS is not available"),
    ("BINMODE", "binary mode is not supported"),
    (
        "LINT",
        "dynamic lint checking is not supported; use `zawk lint`",
    ),
    ("TEXTDOMAIN", "internationalization is not supported"),
    ("SYMTAB", "indirect variable access is not supported"),
    ("FUNCTAB", "indirect function calls are not supported"),
];

/// Reports what the token stream shows on its own: gawk variables zawk ignores and `f`/`r` used as
/// names. Lexer errors are left to the parser.
fn scan_tokens(prog: &str) -> Vec<Diagnostic> {
    let toks: Vec<_> = lexer::Tokenizer::new(prog).map_while(Result::ok).collect();
    let mut diags = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    // `f` and `r` as names clash with f-strings and raw strings: `f"x"` is an f-string and `r"x"`
    // (or `r#"x"#`) a raw string, not the name followed by "x". Reported once per name and kind.
    let mut names_reported: Vec<(&str, &str)> = Vec::new();
    for (loc, tok, _) in &toks {
        let (name, kind) = match tok {
            Tok::Ident(n @ ("f" | "r")) => (*n, "variable"),
            Tok::FunDec(n @ ("f" | "r")) | Tok::CallStart(n @ ("f" | "r")) => (*n, "function"),
            _ => continue,
        };
        if names_reported.contains(&(name, kind)) {
            continue;
        }
        names_reported.push((name, kind));
        let (what, example) = if name == "f" {
            ("f-strings", "`f\"...\"` is an f-string")
        } else {
            ("raw strings", "`r\"...\"` and `r#\"...\"#` are raw strings")
        };
        diags.push(warning(
            "W103",
            format!(
                "`{}` should not be used as a {} name: it clashes with {} ({}, not `{}` followed by a string); rename it",
                name, kind, what, example, name
            ),
            Some(*loc),
        ));
    }
    for (loc, tok, _) in &toks {
        let (lit, open) = match tok {
            Tok::StrLit(lit) => (*lit, "\""),
            Tok::FStrLit(lit) => (*lit, "f\""),
            _ => continue,
        };
        for (i, c) in lexer::unknown_escapes(lit) {
            // `\{` and `\}` are the braces of an f-string.
            if open == "f\"" && matches!(c, '{' | '}') {
                continue;
            }
            let hint = if ".[]()*+?{}|^$".contains(c) {
                format!(
                    ": in a dynamic regex `{}` is then an operator, write `\\\\{}` to match it literally",
                    c, c
                )
            } else {
                String::new()
            };
            diags.push(warning(
                "W102",
                format!(
                    "escape sequence `\\{}` is treated as plain `{}`{}",
                    c, c, hint
                ),
                Some(loc.advance(open).advance(&lit[..i])),
            ));
        }
    }
    for (loc, tok, _) in &toks {
        let Tok::Ident(name) = tok else { continue };
        if let Some((_, why)) = IGNORED_GAWK_VARIABLES.iter().find(|(n, _)| n == name)
            && !reported.contains(name) {
                reported.push(name);
                diags.push(Diagnostic {
                    code: "W101",
                    severity: Severity::Warning,
                    message: format!(
                        "gawk variable `{}` has no special meaning in zawk: {}",
                        name, why
                    ),
                    loc: Some(*loc),
                });
            }
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(prog: &str) -> Value {
        let src = Source {
            name: None,
            text: prog.to_string(),
            line: 0,
        };
        serde_json::from_str(&lint(&[src], &Options::default()).2).unwrap()
    }

    fn codes(v: &Value) -> Vec<&str> {
        v["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["code"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn valid_program() {
        let v = run("BEGIN { x = 1 } { print $1, x }");
        assert_eq!(v["valid"], true);
        assert!(codes(&v).is_empty());
    }

    #[test]
    fn syntax_error_location() {
        let v = run("BEGIN {\n  print 1 +\n}");
        assert_eq!(v["valid"], false);
        let d = &v["diagnostics"][0];
        assert_eq!(d["code"], "E101");
        assert_eq!(d["line"], 2);
        assert_eq!(d["column"], 12);
        assert_eq!(d["source"], "  print 1 +");
        assert_eq!(d["caret"], "           ^");
        assert!(d["message"].as_str().unwrap().starts_with("unexpected"));
    }

    #[test]
    fn lexer_errors() {
        assert_eq!(codes(&run("BEGIN { x = \"abc }")), ["E102"]);
        assert_eq!(codes(&run("@include x.awk")), ["E201"]);
    }

    #[test]
    fn field_operand_operators() {
        for prog in [
            "{ print $$1 }",
            "{ print $++i }",
            "{ print $--i }",
            "{ print $-1 }",
            "{ print $ !0 }",
            "{ print $+1 }",
        ] {
            let v = run(prog);
            assert_eq!(codes(&v), ["E201"], "{}", prog);
            assert!(
                message(&v, 0).contains("parenthesize the field number"),
                "{}",
                message(&v, 0)
            );
        }
        assert_eq!(run("{ print $$1 }")["diagnostics"][0]["column"], 10);
        // Parenthesized operands, and operators after a whole field reference, are fine.
        for prog in [
            "{ print $($1), $(++i), $(-1 + NF), $(!0) }",
            "{ print $1 - 1, ($NF) + 1, $ 1, -$1, !$1 }",
        ] {
            assert_eq!(run(prog)["errors"], 0, "{}", prog);
        }
    }

    #[test]
    fn compile_errors() {
        let v = run("BEGIN {\n  print foo(1)\n}");
        assert_eq!(codes(&v), ["E301"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        // Converting an array with string keys to CSV is caught with its location.
        let v = run("BEGIN { a[\"k\"] = 1; print to_csv(a) }");
        assert_eq!(codes(&v), ["E300"]);
        assert_eq!(v["diagnostics"][0]["column"], 27);
        let v = run("BEGIN { dcgettext(\"a\") }");
        assert_eq!(codes(&v), ["E203"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("internationalization")
        );
        assert_eq!(codes(&run("BEGIN { patsplit(\"a\", b) }")), ["E302"]);
        // gawk functions zawk implements are checked like any other builtin.
        assert_eq!(codes(&run("BEGIN { print and(1) }")), ["E302"]);
        assert!(codes(&run("BEGIN { print isarray(x), compl(1), mkbool(1) }")).is_empty());
        assert_eq!(codes(&run("BEGIN { a[1][2] = 3 }")), ["E202"]);
        assert_eq!(codes(&run("BEGIN { print length(1, 2) }")), ["E302"]);
        assert_eq!(codes(&run("BEGIN { x = 1; x[1] = 2 }")), ["E303"]);
        // Compile errors are located where they occur, not at the first use of the name.
        let v = run("BEGIN {\n  a[1] = 1\n  print a\n}");
        assert_eq!(codes(&v), ["E303"]);
        assert_eq!(v["diagnostics"][0]["line"], 3);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        let v = run("BEGIN { substr(1) }");
        assert_eq!(
            v["diagnostics"][0]["message"],
            "function `substr` expects 2 or 3 arguments, got 1"
        );
    }

    #[test]
    fn unknown_escapes() {
        let v = run(r#"{ print "a\qb", "x\/y\t\\.", f"\{x\}"; match($0, "a\.b") }"#);
        assert_eq!(codes(&v), ["W102", "W102", "W102"]);
        assert_eq!(v["diagnostics"][0]["column"], 11);
        assert_eq!(v["diagnostics"][1]["column"], 19);
        assert!(
            message(&v, 2).contains("write `\\\\.` to match it literally"),
            "{}",
            message(&v, 2)
        );
    }

    #[test]
    fn warnings() {
        let v = run("BEGIN { FIELDWIDTHS = \"2 3\" }");
        assert_eq!(v["valid"], true);
        assert_eq!(codes(&v), ["W101"]);
    }

    #[test]
    fn index_regex() {
        let v = run("{ print index(/a/, $1) }");
        assert_eq!(codes(&v), ["W205"]);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        // As the second argument it is a compile error, not a warning.
        assert_eq!(codes(&run("BEGIN { print index(\"abc\", /b/) }")), ["E300"]);
        assert!(
            codes(&run(
                "BEGIN { print index(\"abc\", \"b\"), match(\"abc\", /b/) }"
            ))
            .is_empty()
        );
    }

    #[test]
    fn invalid_string_regexes() {
        let v = run("BEGIN { x = \"abc\" ~ \"[\"; print x }");
        assert_eq!(codes(&v), ["E307"]);
        assert_eq!(v["valid"], false);
        assert_eq!(v["diagnostics"][0]["column"], 21);
        assert_eq!(message(&v, 0), "invalid regex /[/: unbalanced [");
        for prog in [
            "BEGIN { s = \"a\"; gsub(\"+\", \"x\", s); print s }",
            "BEGIN { s = \"a\"; sub(\"(\", \"x\", s); print s }",
            "BEGIN { print gensub(\"(\", \"x\", \"g\", \"a\") }",
            "BEGIN { print match(\"a\", \"(\") }",
            "BEGIN { print split(\"a\", arr, \"[a\") }",
            "BEGIN { print (\"a\" !~ \"a\" \"(\") }",
            "$0 ~ \"*\" { print }",
        ] {
            assert_eq!(codes(&run(prog)), ["E307"], "{}", prog);
        }
        // Valid patterns, and `split` separators that are not regexes.
        for prog in [
            "BEGIN { print (\"a\" ~ \"[a]\"), match(\"a\", \"a+\") }",
            "BEGIN { print split(\"a[b\", arr, \"[\"), split(\"a+b\", arr, \"+\") }",
            "BEGIN { print split(\"a b\", arr, \" \"), split(\"ab\", arr, \"\") }",
            "BEGIN { s = \"a\"; gsub(\"\\\\+\", \"x\", s); print s }",
        ] {
            assert!(codes(&run(prog)).is_empty(), "{}: {:?}", prog, run(prog));
        }
    }

    #[test]
    fn array_key_comparisons() {
        let v = run("BEGIN { a[10] = 1; for (k in a) if (k > 9) print k }");
        assert_eq!(codes(&v), ["W108"]);
        assert_eq!(v["diagnostics"][0]["column"], 37);
        assert_eq!(
            codes(&run(
                "BEGIN { a[1] = 1; for (k in a) if (-1 <= k) print k }"
            )),
            ["W108"]
        );
        assert!(
            codes(&run(
                "BEGIN { a[1] = 1; for (k in a) if (k+0 > 9) print k }"
            ))
            .is_empty()
        );
        assert!(codes(&run("BEGIN { a[1] = 1; for (k in a) if (k == 1) print k }")).is_empty());
        assert!(
            codes(&run(
                "BEGIN { a[1] = 1; for (k in a) n++; if (k > 9) print k, n }"
            ))
            .is_empty()
        );
    }

    #[test]
    fn not_assignable() {
        let v = run(r#"BEGIN { gsub(/a/, "b", substr("x", 1)) }"#);
        assert_eq!(codes(&v), ["E306"]);
        assert_eq!(v["diagnostics"][0]["column"], 24);
        let msg = v["diagnostics"][0]["message"].as_str().unwrap();
        assert!(
            msg.contains("`substr(...)`") && !msg.contains("Call("),
            "{}",
            msg
        );
        let v = run(r#"BEGIN { if (substr("abc", 1, 1) = "a") print }"#);
        assert_eq!(codes(&v), ["E306"]);
        assert_eq!(v["diagnostics"][0]["column"], 13);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("`==`")
        );
        assert_eq!(codes(&run("BEGIN { print 2--1 }")), ["E306"]);
    }

    #[test]
    fn function_name_conflicts() {
        let v = run("function g(a, a) { return a }\nBEGIN { print g(1, 2) }");
        assert_eq!(codes(&v), ["E305"]);
        assert_eq!(codes(&run("function h(h) { return 1 }")), ["E305"]);
        assert_eq!(
            codes(&run(
                "function h(x) { return x }\nfunction g(h) { return h }"
            )),
            ["E305"]
        );
        let v = run("function foo(x) { return x }\nBEGIN {\n  foo = 1; print foo(2) }");
        assert_eq!(codes(&v), ["E305"]);
        assert_eq!(v["diagnostics"][0]["line"], 3);
        assert_eq!(
            codes(&run("function foo(x) { return x }\nBEGIN { foo[1] = 1 }")),
            ["W207", "E305"]
        );
        assert!(codes(&run("function foo(x) { return x }\nBEGIN { print foo(1) }")).is_empty());
    }

    #[test]
    fn read_only() {
        let v = run("BEGIN { ARGV[1] = \"x\"; ARGC++; delete ARGV }");
        assert_eq!(v["valid"], false);
        assert_eq!(codes(&v), ["E305", "E305", "E305"]);
        let v = run("{ FILENAME = \"a\"; NR = 1; NF -= 1 }");
        assert_eq!(codes(&v), ["E305", "E305", "E305"]);
        assert_eq!(v["diagnostics"][1]["column"], 19);
        let v = run("{\n  $0 = tolower($0); $1 = \"\"; $NF++; getline $2 }");
        assert_eq!(codes(&v), ["E305", "E305", "E305", "E305"]);
        assert_eq!(v["diagnostics"][1]["line"], 2);
        assert_eq!(v["diagnostics"][1]["column"], 21);
        let header = Options {
            header: true,
            ..Options::default()
        };
        let v = run_with("{ $FI[\"name\"] = \"x\"; FI[\"name\"] = 1 }", &header);
        assert_eq!(codes(&v), ["E305", "E305"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("$FI[...]")
        );
        // Reading them is fine.
        assert!(
            codes(&run_with(
                "{ print ARGV[1], ARGC, FILENAME, NR, NF, $0, $1, $FI[\"name\"] }",
                &header
            ))
            .is_empty()
        );
    }

    #[test]
    fn f_name() {
        let v = run(
            "function f(x) { return x }\nfunction g(y) { return f(y) }\nBEGIN { f = 1; print g(f) }",
        );
        assert_eq!(codes(&v), ["W103", "W103", "E305"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("function")
        );
        assert_eq!(v["diagnostics"][1]["line"], 3);
        assert_eq!(v["diagnostics"][1]["column"], 9);
        assert!(codes(&run("BEGIN { x = fx = xf = 1; print f\"{x}\", fx, xf }")).is_empty());
    }

    #[test]
    fn r_name() {
        let v = run("BEGIN {\n  r = 5; print r\"x\", r\n}");
        assert_eq!(codes(&v), ["W103"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("raw strings")
        );
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 3);
        let v = run("function r(x) { return x }\nBEGIN { print r(1), r(2) }");
        assert_eq!(codes(&v), ["W103"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("function")
        );
        // Both names in one program: one warning per name and kind.
        assert_eq!(
            codes(&run("BEGIN { f = r = 1; print f, r }")),
            ["W103", "W103"]
        );
        assert!(
            codes(&run(
                "BEGIN { rx = xr = 1; print r\"\\d\", r#\"a\"b\"#, rx, xr }"
            ))
            .is_empty()
        );
    }

    fn run_with(prog: &str, opts: &Options) -> Value {
        let src = Source {
            name: None,
            text: prog.to_string(),
            line: 0,
        };
        serde_json::from_str(&lint(&[src], opts).2).unwrap()
    }

    #[test]
    fn unset_variables() {
        let v = run("BEGIN { x = 1 }\n{ print x, y }");
        assert_eq!(v["valid"], true);
        assert_eq!(codes(&v), ["W201"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 12);
        // Set by a function, a loop, getline, -v, or a function parameter: not reported.
        assert!(codes(&run("{ n = split($0, parts); print parts[1], n }")).is_empty());
        assert!(codes(&run("{ for (k in a) print k; a[$1]++ }")).is_empty());
        assert!(codes(&run("{ getline line; print line, NR, FILENAME }")).is_empty());
        assert!(codes(&run("function g(a, b) { return a + b } { print g($1) }")).is_empty());
        let opts = Options {
            assigned: vec!["n".into()],
            ..Options::default()
        };
        assert!(codes(&run_with("{ print n }", &opts)).is_empty());
        assert!(codes(&run("# @var n count\n# @var m? other\n{ print n, m }")).is_empty());
        // An array that is only read is empty too.
        assert_eq!(codes(&run("{ if ($1 in seen) print }")), ["W201"]);
    }

    #[test]
    fn likely_mistakes() {
        let v = run("{ print $NF-1 }");
        assert_eq!(codes(&v), ["W116"]);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        assert_eq!(codes(&run("{ print $NF+2 }")), ["W116"]);
        assert!(codes(&run("{ i = 1; print $(NF-1), $NF, $NF+0, $i-1 }")).is_empty());
        assert_eq!(codes(&run("$3 > \"100\" { print }")), ["W117"]);
        assert_eq!(
            codes(&run("{ if (\"1.5\" <= $FI[\"x\"]) print }")),
            ["W117", "W105"]
        );
        assert!(codes(&run("$3 > 100 || $1 > \"abc\" || $2 == \"10\" { print }")).is_empty());
        assert_eq!(codes(&run("{ printf $0 }")), ["W118"]);
        assert_eq!(codes(&run("{ printf($1 \"\\n\") }")), ["W118"]);
        assert!(codes(&run("{ printf \"%s\\n\", $0 }")).is_empty());
        let v = run("{ t = dec_add(t, $3 * 1.06) } END { print t, dec_cmp(t - 1, 0) }");
        assert_eq!(codes(&v), ["W123", "W123"]);
        assert!(message(&v, 0).contains("dec_add"));
        assert!(message(&v, 1).contains("dec_cmp"));
        assert!(codes(&run(
            "{ t = dec_add(t, dec_mul($3, \"1.06\")) } END { print dec_round(t, 2), dec_add(t, -1) }"
        ))
        .is_empty());
        let v = run("BEGIN { OFS = \"-\" }\n{ FS = \",\"; print $2 } { FS = \";\" }");
        assert_eq!(codes(&v), ["W119"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert!(codes(&run("BEGIN { FS = \",\" } { print $2 }")).is_empty());
        let v = run("BEGIN { if (\"0\") print; while (\"\") break }\n\"x\" { print }");
        assert_eq!(codes(&v), ["W120", "W120", "W120"]);
        assert!(message(&v, 0).contains("zawk treats it as false"));
        assert!(message(&v, 1).contains("always false"));
        assert!(message(&v, 2).contains("always true"));
        assert_eq!(v["diagnostics"][2]["line"], 2);
        assert_eq!(v["diagnostics"][2]["column"], 1);
    }

    #[test]
    fn assignment_order() {
        // The subscript and the right-hand side affect each other: zawk and gawk disagree.
        let v = run("BEGIN { i = 1; a[i++] = i; print a[1] }");
        assert_eq!(codes(&v), ["W122"]);
        assert_eq!(v["diagnostics"][0]["column"], 16);
        assert_eq!(codes(&run("BEGIN { a[i++] += i; print a[1] }")), ["W122"]);
        assert_eq!(
            codes(&run("BEGIN { a[x] = (x = 2); print a[2] }")),
            ["W122"]
        );
        assert_eq!(
            codes(&run(
                "function p(x) { return x }\nBEGIN { a[p(1)] = p(2); print a[1] }"
            )),
            ["W122"]
        );
        assert_eq!(
            codes(&run(
                "function p(x) { return x }\n{ a[n++] = p($1) } END { print n }"
            )),
            ["W122"]
        );
        // Independent sides: the order does not matter.
        assert!(codes(&run("{ a[n++] = $0 } END { print n, a[0] }")).is_empty());
        assert!(codes(&run("{ c[$1] = c[$1] + 1 } END { for (k in c) print k }")).is_empty());
        assert!(
            codes(&run(
                "function p(x) { return x }\n{ a[$1] = p($2) } END { for (k in a) print k }"
            ))
            .is_empty()
        );
        assert!(
            codes(&run(
                "{ n = split($0, parts); b[$1] = n } END { for (k in b) print k }"
            ))
            .is_empty()
        );
    }

    #[test]
    fn unused() {
        let v = run("{ totl += $1 }\nEND { print total }");
        assert_eq!(codes(&v), ["W206", "W201"]);
        assert!(message(&v, 0).contains("`total`"));
        assert_eq!(v["diagnostics"][0]["line"], 1);
        assert_eq!(codes(&run("{ n++ }")), ["W206"]);
        // Arrays, loop keys, filled by a function, getline, special variables: not reported.
        assert!(codes(&run("{ a[$1] = 1; n = split($0, p); OFS = \"-\"; getline junk; for (k in a) m++; print m, n }")).is_empty());
        let v = run(
            "function used(x) { return x }\nfunction unused(x) { return x }\n{ print used($1) }",
        );
        assert_eq!(codes(&v), ["W207"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        // Formats built from literals are checked too.
        assert_eq!(
            codes(&run("BEGIN { printf \"%s \" \"%s\\n\", 1 }")),
            ["W202"]
        );
        assert_eq!(
            codes(&run("BEGIN { print sprintf(\"%d\" \"\", 1, 2) }")),
            ["W203"]
        );
    }

    #[test]
    fn format_arguments() {
        assert_eq!(super::format_arguments(b"%s %d%%\n"), 2);
        assert_eq!(super::format_arguments(b"%-*.*f|%5.2e %lld"), 5);
        assert_eq!(super::format_arguments(b"%2$s %1$s"), 2);
        assert_eq!(super::format_arguments(b"%1$s %1$s"), 1);
        assert_eq!(super::format_arguments(b"%2$*3$d"), 3);
        let v = run("BEGIN {\n  printf \"%s %s\\n\", \"a\"\n}");
        assert_eq!(codes(&v), ["W202"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 3);
        assert_eq!(
            codes(&run("BEGIN { print sprintf(\"%d\", 1, 2) }")),
            ["W203"]
        );
        assert_eq!(codes(&run("BEGIN { printf(\"%s\") }")), ["W202"]);
        assert!(
            codes(&run(
                "BEGIN { printf \"%s=%d\\n\", \"a\", 1; x = \"%s\"; printf x }"
            ))
            .is_empty()
        );
    }

    #[test]
    fn header_columns() {
        let prog = "BEGIN { n = 1; print n }\n{ print $FI[\"name\"], FI[\"age\"] }";
        let v = run(prog);
        assert_eq!(codes(&v), ["W105"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 10);
        let header = Options {
            header: true,
            ..Options::default()
        };
        assert!(codes(&run_with(prog, &header)).is_empty());
    }

    fn with_lines(lines: &[&str], header: bool, fs: Option<&str>) -> Options {
        let sample = Sample::Lines(lines.iter().map(|l| l.to_string()).collect());
        Options {
            header,
            field_sep: fs.map(String::from),
            data: Some(Data {
                file: "data.csv".to_string(),
                sample,
            }),
            ..Options::default()
        }
    }

    fn message(v: &Value, i: usize) -> &str {
        v["diagnostics"][i]["message"].as_str().unwrap()
    }

    #[test]
    fn data_unknown_columns() {
        let opts = with_lines(&["name,age:INT64,price", "a,1,2"], true, Some(","));
        let v = run_with(
            "{ print $FI[\"nmae\"], $FI[\"Price\"], $FI[\"age\"], $FI[\"zzz\"], $FI[\"nmae\"] }",
            &opts,
        );
        assert_eq!(codes(&v), ["W126", "W126", "W126", "W126"]);
        assert_eq!(v["valid"], false);
        assert!(message(&v, 0).contains("did you mean `name`?"));
        assert!(message(&v, 1).contains("did you mean `price`?"));
        assert!(message(&v, 2).contains("did you mean `age:INT64`?"));
        assert!(message(&v, 3).contains("the columns are `name`, `age:INT64`, `price`"));
        assert_eq!(v["diagnostics"][0]["column"], 10);
        assert!(
            codes(&run_with(
                "{ k = \"x\"; print $FI[\"name\"], $FI[k] }",
                &opts
            ))
            .is_empty()
        );
        let jsonl = Options {
            header: true,
            data: Some(Data {
                file: "x.jsonl".to_string(),
                sample: Sample::Fields {
                    header: Some(vec!["id".into(), "user".into()]),
                    rows: vec![vec!["1".into(), "a".into()]],
                },
            }),
            ..Options::default()
        };
        assert_eq!(codes(&run_with("{ print $FI[\"usr\"] }", &jsonl)), ["W126"]);
    }

    #[test]
    fn data_fields_beyond_nf() {
        let opts = with_lines(&["a b c", "d e", ""], false, None);
        let v = run_with("{ print $3, $4, $4 }", &opts);
        assert_eq!(codes(&v), ["W127"]);
        assert!(message(&v, 0).contains("at most 3 fields"));
        // The record may be read from elsewhere.
        assert!(codes(&run_with("{ getline; print $4 }", &opts)).is_empty());
        // In BEGIN, fields are reported by W218.
        assert_eq!(codes(&run_with("BEGIN { print $4 }", &opts)), ["W218"]);
    }

    #[test]
    fn data_wrong_separator() {
        let opts = with_lines(&["a,b,c", "d,e,f"], false, None);
        let v = run_with("{ print $1; print $2 }", &opts);
        assert_eq!(codes(&v), ["W128"]);
        assert!(message(&v, 0).contains("did you mean `-F,` or `-i csv`?"));
        assert_eq!(v["diagnostics"][0]["column"], 19);
        let tabs = with_lines(&["a\tb", "c\td"], false, Some(","));
        assert!(message(&run_with("{ print $2 }", &tabs), 0).contains("`-F'\\t'` or `-i tsv`"));
        // The separator of BEGIN is the one the run uses.
        assert!(codes(&run_with("BEGIN { FS = \",\" } { print $2 }", &opts)).is_empty());
        assert!(
            codes(&run_with(
                "{ print $2 }",
                &with_lines(&["a,b"], false, Some(","))
            ))
            .is_empty()
        );
        // An unknown separator: no checks.
        assert!(codes(&run_with("BEGIN { x = \",\"; FS = x } { print $2 }", &opts)).is_empty());
        assert!(codes(&run_with("{ print $1 }", &opts)).is_empty());
        // A header read with the wrong separator.
        let header = with_lines(&["name,age", "a,1"], true, None);
        assert_eq!(
            codes(&run_with("{ print $FI[\"age\"] }", &header)),
            ["W128"]
        );
    }

    #[test]
    fn data_string_comparisons() {
        let opts = with_lines(
            &["id,price", "1,12.5", "2,N/A", "3,", "4, 7 "],
            true,
            Some(","),
        );
        let v = run_with(
            "$2 > 10 || 5 <= $FI[\"price\"] { n++ } $1 > 0 { m++ } END { print n, m }",
            &opts,
        );
        assert_eq!(codes(&v), ["W129", "W129"]);
        assert!(
            message(&v, 0)
                .contains("1 record of the first 4 records of `data.csv` (e.g. record 2: \"N/A\")")
        );
        assert!(message(&v, 1).contains("`$FI[\"price\"]` is compared with the number 5"));
        assert!(
            codes(&run_with(
                "$2 + 0 > 10 { n++ } $2 == 10 { m++ } END { print n, m }",
                &opts
            ))
            .is_empty()
        );
        // A header line read as a record.
        let v = run_with(
            "$2 > 10 { print }",
            &with_lines(&["id,price", "1,12.5"], false, Some(",")),
        );
        assert_eq!(codes(&v), ["W129"]);
        assert!(message(&v, 0).contains("looks like a header line"));
        // Hexadecimal and signed inf/nan fields are numbers, as at run time.
        let opts = with_lines(&["id,amt", "1,0x1F", "2,+inf", "3,-nan"], true, Some(","));
        assert!(codes(&run_with("$2 > 5 { print }", &opts)).is_empty());
    }

    #[test]
    fn numeric_redirects() {
        let v = run("{ print $1 > 10 }");
        assert_eq!(codes(&v), ["W114"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("the file `10`")
        );
        assert_eq!(v["diagnostics"][0]["column"], 14);
        assert_eq!(codes(&run("{ printf \"%s\", $1 >> NR + 1 }")), ["W114"]);
        assert_eq!(codes(&run("{ print $1 > -1.5 }")), ["W114"]);
        assert!(codes(&run("{ print ($1 > 10) }")).is_empty());
        assert!(codes(&run("{ print $1 > \"10\" }")).is_empty());
        assert!(codes(&run("{ print $1 > ($2 \".txt\") }")).is_empty());
        assert!(codes(&run("{ print $1 | 10 }")).is_empty());
    }

    #[test]
    fn string_fields() {
        let v = run("{ print $\"age\" }");
        assert_eq!(codes(&v), ["W121"]);
        assert!(message(&v, 0).contains("`$FI[\"age\"]`"));
        assert!(message(&v, 0).contains("`$0`"));
        assert_eq!(codes(&run("{ print $(\"a\" \"ge\") }")), ["W121"]);
        assert_eq!(codes(&run("{ $\"x\" = 1 }")), ["E305", "W121"]);
        assert_eq!(codes(&run("{ print $\"\" }")), ["W121"]);
        let v = run("{ print $\"3abc\" }");
        assert_eq!(codes(&v), ["W121"]);
        assert!(message(&v, 0).contains("leading digits"));
        assert!(codes(&run("{ print $\"2\" }")).is_empty());
        assert!(codes(&run("{ print $\" 2 \" }")).is_empty());
        assert_eq!(codes(&run("{ print $FI[\"age\"] }")), ["W105"]);
        assert!(codes(&run("{ c = \"age\"; print $c }")).is_empty());
    }

    #[test]
    fn string_range_bounds() {
        let v = run("BEGIN { for (i in \"a\"..\"c\") print i }");
        assert_eq!(codes(&v), ["W208", "W208"]);
        assert!(message(&v, 0).contains("the string \"a\""));
        assert_eq!(
            codes(&run("BEGIN { x = \"1\"; for (i in x \"0\"..=20) print i }")),
            ["W208"]
        );
        assert!(codes(&run("BEGIN { for (i in 1..=3) print i }")).is_empty());
        assert!(codes(&run("BEGIN { for (i in \"1\"..\"3\") print i }")).is_empty());
        assert!(codes(&run("BEGIN { n = 3; for (i in 0..n + 1) print i }")).is_empty());
    }

    #[test]
    fn condition_assignments() {
        let v = run("BEGIN { if (x = 1) print x }");
        assert_eq!(codes(&v), ["W115"]);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .contains("`x` is assigned")
        );
        assert_eq!(v["diagnostics"][0]["column"], 13);
        assert_eq!(codes(&run("BEGIN { while (y = 0) {} print y }")), ["W115"]);
        assert_eq!(
            codes(&run("BEGIN { do {} while (!(y = 0)); print y }")),
            ["W115"]
        );
        assert_eq!(
            codes(&run("BEGIN { for (i = 0; i = 3; i++) break; print i }")),
            ["W115"]
        );
        assert_eq!(
            codes(&run("BEGIN { z = 1; x = (z = 2) ? 1 : 2; print x, z }")),
            ["W115"]
        );
        assert_eq!(
            codes(&run("{ a = $1 } a = \"x\" && 1 { print a }")),
            ["W115"]
        );
        assert_eq!(
            codes(&run("BEGIN { a[1] = 0; if (a[1] = 2) print }")),
            ["W115"]
        );
        assert!(codes(&run("BEGIN { if ((x = 1) != \"\") print x }")).is_empty());
        assert!(codes(&run("BEGIN { x = 1; if (x == 1) print x }")).is_empty());
        assert!(codes(&run("BEGIN { while ((getline line) > 0) print line }")).is_empty());
    }

    #[test]
    fn begin_record_reads() {
        let v = run("BEGIN {\n  print $1, $1, NF, FILENAME, $(i)\n  i = 2\n}");
        assert_eq!(codes(&v), ["W218", "W218", "W218", "W218"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .starts_with("`$1`")
        );
        assert!(
            v["diagnostics"][1]["message"]
                .as_str()
                .unwrap()
                .contains("always 0")
        );
        assert!(
            v["diagnostics"][3]["message"]
                .as_str()
                .unwrap()
                .starts_with("a computed field (`$(...)`) is read")
        );
        let v = run("BEGIN { print $(-1), $FI[\"id\"] }");
        assert!(
            v["diagnostics"][0]["message"]
                .as_str()
                .unwrap()
                .starts_with("a field (`$(-1)`) is read")
        );
        assert!(
            v["diagnostics"][1]["message"]
                .as_str()
                .unwrap()
                .starts_with("`$FI[\"id\"]` is read")
        );
        // After `getline`, or outside BEGIN, there is a record.
        assert!(codes(&run("BEGIN { getline; print $1, NF }")).is_empty());
        assert!(codes(&run("BEGIN { while ((getline < \"f\") > 0) print $0 }")).is_empty());
        assert!(
            codes(&run(
                "function rd() { return getline } BEGIN { rd(); print $0 }"
            ))
            .is_empty()
        );
        assert!(codes(&run("{ print $1, NF, FILENAME } END { print $0, NF }")).is_empty());
    }

    #[test]
    fn extra_arguments() {
        let v = run("function g(a) { return a }\nBEGIN { print g(1, 2, 3) }");
        assert_eq!(codes(&v), ["W204"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 15);
        assert!(codes(&run("function g(a, b) { return a } BEGIN { print g(1) }")).is_empty());
    }

    #[test]
    fn parallel() {
        let prog = "NR == 1 { print }\n{ last = $1; sum += $1; n++; avg = avg + $2; seen[$1] = $2 }\nEND { print last, sum, n, avg, length(seen) }";
        assert!(codes(&run(prog)).is_empty());
        let pr = Options {
            parallel: Some(Parallel::Record),
            ..Options::default()
        };
        let v = run_with(prog, &pr);
        assert_eq!(codes(&v), ["W301", "W302"]);
        assert_eq!(v["diagnostics"][1]["line"], 2);
        assert_eq!(v["diagnostics"][1]["column"], 3);
        // Values collected per worker in PREPARE are the documented fix.
        let fixed = "{ max = max > $1 ? max : $1 } PREPARE { m[PID] = max } END { for (p in m) print m[p] }";
        assert!(codes(&run_with(fixed, &pr)).is_empty());
        // A run with `-p` prints the W302/W303 warnings (not W301) to stderr, with their place.
        let src = Source {
            text: prog.to_string(),
            ..Source::default()
        };
        let w = parallel_warnings(std::slice::from_ref(&src), Parallel::Record, false);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].starts_with("zawk: warning: -p r: line 2, column 3: W302 "),
            "{}",
            w[0]
        );
        assert!(
            parallel_warnings(
                &[Source {
                    text: fixed.to_string(),
                    ..Source::default()
                }],
                Parallel::File,
                false
            )
            .is_empty()
        );
        assert!(
            parallel_warnings(
                &[Source {
                    text: "{ x = ".to_string(),
                    ..Source::default()
                }],
                Parallel::File,
                false
            )
            .is_empty()
        );
        let pf = Options {
            parallel: Some(Parallel::File),
            ..Options::default()
        };
        // Array elements assigned in main and read in END are merged by summing (W303); keys
        // only, counts and sums are fine.
        assert_eq!(
            codes(&run_with(
                "{ a[$4] = 1 } END { for (k in a) s += a[k]; print s }",
                &pr
            )),
            ["W303"]
        );
        assert_eq!(
            codes(&run_with(
                "{ last[$1] = $2 } END { for (k in last) print k, last[k] }",
                &pr
            )),
            ["W303"]
        );
        assert_eq!(
            codes(&run_with(
                "function show(x, k) { for (k in x) print x[k] } { m[$1] = $2 } END { show(m) }",
                &pr
            )),
            ["W303"]
        );
        for ok in [
            "{ a[$4] = 1 } END { for (k in a) n++; print n, length(a) }",
            "{ c[$1]++; s[$1] += $2; t[$1] = t[$1] + $3 } END { for (k in c) print c[k], s[k], t[k] }",
            "{ a[$1] = $2 } END { print length(a) }",
        ] {
            assert!(codes(&run_with(ok, &pr)).is_empty(), "{}", ok);
        }
        assert!(
            codes(&run(
                "{ a[$4] = 1 } END { for (k in a) s += a[k]; print s }"
            ))
            .is_empty()
        );
        assert!(codes(&run_with("FNR == 1 { print FILENAME }", &pf)).is_empty());
        assert_eq!(
            codes(&run_with("FNR == 1 { print FILENAME }", &pr)),
            ["W301"]
        );
    }

    #[test]
    fn parallel_cross_record_state() {
        let pr = Options {
            parallel: Some(Parallel::Record),
            ..Options::default()
        };
        // Range state is per worker (W304, `-pr` only: with `-pf` each file is read by one worker).
        assert_eq!(codes(&run_with("/BEGIN/, /END/", &pr)), ["W304"]);
        let pf = Options {
            parallel: Some(Parallel::File),
            ..Options::default()
        };
        assert!(codes(&run_with("/BEGIN/, /END/", &pf)).is_empty());
        // State carried over from earlier records that decides or makes output (W305).
        for prog in [
            "!seen[$2]++ { print $2 }",
            "$1 != prev { print } { prev = $1 }",
            "{ n++; print n }",
            "{ s += $1; print s }",
            "{ if (!($1 in seen)) { seen[$1] = 1; print } }",
            "$1 > m { m = $1; print }",
        ] {
            assert_eq!(codes(&run_with(prog, &pr)), ["W305"], "{}", prog);
            assert!(codes(&run(prog)).is_empty(), "{}", prog);
        }
        // Values of this record, and accumulations that only feed themselves (or END: W302/W303).
        for ok in [
            "{ x = $1; print x }",
            "{ split($0, a); print a[1] }",
            "{ a[\"k\"] = $1; print a[\"k\"] }",
            "{ delete a; a[$1] = 1; for (k in a) print k }",
            "{ c[$1]++ } END { for (k in c) print k, c[k] }",
            "{ if ($1 > max) max = $1 } PREPARE { m[PID] = max } END { for (p in m) print m[p] }",
            "{ getline line < \"f\"; print line }",
        ] {
            assert!(codes(&run_with(ok, &pr)).is_empty(), "{}", ok);
        }
        // A name W302 reports is not reported again.
        assert_eq!(
            codes(&run_with("$1 > m { m = $1; print } END { print m }", &pr)),
            ["W302"]
        );
        // A run prints W304/W305; with `--keep-order` W301 too.
        let src = |t: &str| Source {
            text: t.to_string(),
            ..Source::default()
        };
        let w = parallel_warnings(&[src("!seen[$1]++")], Parallel::Record, false);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].starts_with("zawk: warning: -p r: line 1, column 2: W305 "),
            "{}",
            w[0]
        );
        assert!(parallel_warnings(&[src("NR % 10 == 0")], Parallel::Record, false).is_empty());
        let w = parallel_warnings(&[src("NR % 10 == 0")], Parallel::Record, true);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].starts_with("zawk: warning: --keep-order: line 1, column 1: W301 "),
            "{}",
            w[0]
        );
        assert!(has_range_pattern(&[src("NR == 2, /x/ { print }")]));
        assert!(!has_range_pattern(&[src("/x/ { print }")]));
        assert!(!has_range_pattern(&[src("{ x = ")]));
    }

    #[test]
    fn multiple_sources() {
        let srcs = [
            Source {
                name: Some("a.awk".into()),
                text: "function g(x) { return x }".into(),
                line: 0,
            },
            Source {
                name: Some("b.awk".into()),
                text: "BEGIN {\n  print g(1\n}".into(),
                line: 0,
            },
        ];
        let v: Value = serde_json::from_str(&lint(&srcs, &Options::default()).2).unwrap();
        let d = &v["diagnostics"][0];
        assert_eq!(d["file"], "b.awk");
        assert_eq!(d["line"], 2);
    }

    #[test]
    fn named_inline_sources() {
        // `zawk lint` names `-e` sources `-e#N` when there are several sources, as a run does.
        let srcs = [
            Source {
                name: Some("a.awk".into()),
                text: "BEGIN { x = 1 }".into(),
                line: 0,
            },
            Source {
                name: Some(parsing::inline_source_name(1)),
                text: "BEGIN {\n  print 1 +\n}".into(),
                line: 0,
            },
        ];
        let v: Value = serde_json::from_str(&lint(&srcs, &Options::default()).2).unwrap();
        let d = &v["diagnostics"][0];
        assert_eq!(d["file"], "-e#1");
        assert_eq!(d["line"], 2);
    }
}
