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
use crate::cfg::{self, Escaper};
use crate::common::Stage;
use crate::builtins::Function;
use crate::common::Either;
use crate::compile;
use crate::lexer::{self, Loc, Tok};
use crate::parsing;
use lalrpop_util::ParseError;
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{json, Value};

/// A part of the program: a `-f` file, a `-e` string or the program operand.
pub struct Source {
    /// The file name, or `None` for a program given on the command line.
    pub name: Option<String>,
    pub text: String,
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
    /// `-A`: allow user input in commands.
    pub allow_arbitrary_commands: bool,
    /// `-p`: the program runs in parallel.
    pub parallel: Option<Parallel>,
    /// Variables set on the command line with `-v`.
    pub assigned: Vec<String>,
    /// `-H` (or an input format that implies it): `FI` maps the column names of the header.
    pub header: bool,
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
    assigned.extend(sources.iter().flat_map(|s| crate::awk_util::declared_vars(&s.text)));
    let opts = Options { assigned, ..*opts };
    diags.extend(check_program(&prog, &opts));
    diags.sort_by_key(|d| d.loc.map_or((usize::MAX, 0), |l| (l.line, l.col)));

    let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
    let lines: Vec<&str> = prog.lines().collect();
    let items: Vec<Object> = diags
        .iter()
        .map(|d| to_json(d, &lines, sources, &starts))
        .collect();
    let warnings = diags.len() - errors;
    (errors, warnings, report(errors, warnings, &items))
}

/// The report for a program file that cannot be read.
pub fn file_error(file: &str, reason: &str) -> String {
    let d = Object(vec![
        ("code", json!("E001")),
        ("severity", json!("error")),
        ("message", json!(format!("can't open source file for reading: {}", reason))),
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
    v.0.extend([
        ("file", json!(file)),
        ("line", json!(loc.line - starts.get(idx).copied().unwrap_or(0) + 1)),
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
    let mut diags = check_ast(&program, opts);
    diags.extend(compile_program(&a, text, program, opts.allow_arbitrary_commands));
    diags
}

/// Builds and type checks a parsed program, returning the first error found.
fn compile_program<'a>(
    a: &'a Arena,
    text: &str,
    program: ast::Prog<'a, 'a, &'a str>,
    allow_arbitrary_commands: bool,
) -> Option<Diagnostic> {
    let program = a.alloc(program);
    // The type checker reports some unsupported programs with a panic: report it as an error
    // instead of crashing (and keep the panic message off stderr).
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ctx = cfg::ProgramContext::from_prog(a, program, Escaper::Identity)?;
        ctx.allow_arbitrary_commands = allow_arbitrary_commands;
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
        ParseError::UnrecognizedToken { token: (l, _, _), .. } => ("E101", *l),
        ParseError::ExtraToken { token: (l, _, _) } => ("E101", *l),
        ParseError::User { error } if error.desc.contains("unsupported") => ("E201", error.location),
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
        "patsplit",
        "loop with match(): `while (match(s, re)) { a[++n] = substr(s, RSTART, RLENGTH); s = substr(s, RSTART + RLENGTH) }`",
    ),
    ("bindtextdomain", "internationalization is not supported; drop the call"),
    ("dcgettext", "internationalization is not supported; use the string itself"),
    ("dcngettext", "internationalization is not supported; pick the singular or plural string with `n == 1 ? s1 : s2`"),
];

fn compile_diagnostic(prog: &str, msg: &str, loc: Option<Loc>) -> Diagnostic {
    let msg = msg.trim_start_matches("zawk: ").trim_start_matches("fatal: ");
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
    } else if msg.contains("user input") || msg.contains("arbitrary") {
        ("E304", false)
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
        if let Some((_, rest)) = msg.split_once(q) {
            if let Some((name, _)) = rest.split_once(if q == '`' { '`' } else { '"' }) {
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return Some(name);
                }
            }
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
    /// `printf` and `sprintf` with a literal format: the format, the number of arguments to
    /// format and the location.
    formats: Vec<(&'a [u8], usize, Option<Loc>)>,
    /// Assignments to read-only variables and fields: the message and the location.
    read_only: Vec<(String, Option<Loc>)>,
    /// Reads of `FI[...]` (e.g. `$FI["name"]`): the location.
    header_columns: Vec<Option<Loc>>,
    /// Reads of fields, `NF` and `FILENAME` in BEGIN: what is read and the location.
    begin_record_reads: Vec<(String, Option<Loc>)>,
    /// Whether BEGIN or a function reads a record with `getline`.
    getline: bool,
}

/// Special variables a program may read but not assign, with why.
const READ_ONLY_VARIABLES: &[(&str, &str)] = &[
    ("ARGC", "changing it does not change the input files"),
    ("ARGV", "changing it does not change the input files"),
    ("FILENAME", "it is the name of the input file being read"),
    ("NR", "it counts the records read"),
    ("NF", "it is the number of fields of the current record"),
    ("FI", "it maps the column names of the header to field indexes"),
];

/// Builtins that only read their arguments; a variable passed to any other function may be
/// filled by it (e.g. `split(s, parts)`, or an array passed to a user function).
const READ_ONLY_FUNCTIONS: &[&str] = &[
    "length", "substr", "index", "sprintf", "sin", "cos", "atan2", "exp", "log", "sqrt", "int",
    "srand", "tolower", "toupper", "system", "close", "fflush", "strtonum", "gensub", "and",
    "or", "xor", "compl", "lshift", "rshift", "strftime", "mktime", "trim", "ltrim", "rtrim",
    "max", "min", "abs", "uuid",
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
            self.uses.push(Use { name, region: self.region, loc });
        }
    }

    fn set(&mut self, name: &'a str, kind: SetKind, loc: Option<Loc>) {
        if !self.is_local(name) {
            self.sets.push(Set { name, region: self.region, kind, loc });
        }
    }

    fn array(&mut self, e: &AstExpr<'a>) {
        if let ast::Expr::Var(name) = e {
            if !self.is_local(name) && !self.arrays.contains(name) {
                self.arrays.push(name);
            }
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
                if let Some((e, _)) = out {
                    self.expr(e);
                }
            }
            Printf(fmt, args, out) => {
                if let ast::Expr::StrLit(f) = fmt {
                    self.formats.push((f, args.len(), self.loc(fmt)));
                }
                self.expr(fmt);
                args.iter().for_each(|e| self.expr(e));
                if let Some((e, _)) = out {
                    self.expr(e);
                }
            }
            If(c, t, f) => {
                self.expr(c);
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
                    self.expr(e);
                }
                if let Some(s) = update {
                    self.stmt(s);
                }
                self.stmt(body);
            }
            DoWhile(c, body) | While(_, c, body) => {
                self.expr(c);
                self.stmt(body);
            }
            ForEach(key, arr, body) => {
                self.set(key, SetKind::Indirect, None);
                self.array(arr);
                self.expr(arr);
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

    /// Walks an assignment target.
    fn lvalue(&mut self, e: &'a AstExpr<'a>, kind: SetKind) {
        self.check_read_only(e);
        match e {
            ast::Expr::Var(name) => self.set(name, kind, self.loc(e)),
            ast::Expr::Index(arr, ix) => {
                self.array(arr);
                match arr {
                    ast::Expr::Var(name) => self.set(name, SetKind::Indirect, self.loc(arr)),
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
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | ReadStdin | Cond(_) => {}
            Var(name) => {
                if self.region == Region::Begin && matches!(*name, "NF" | "FILENAME") {
                    self.begin_record_reads.push((format!("`{}`", name), self.loc(e)));
                }
                self.used(name, self.loc(e))
            }
            Unop(op, x) => {
                if self.region == Region::Begin && matches!(op, ast::Unop::Column) {
                    let what = match x {
                        ILit(n) => format!("`${}`", n),
                        _ => "fields".to_string(),
                    };
                    self.begin_record_reads.push((what, self.loc(e)));
                }
                self.expr(x)
            }
            Binop(_, l, r) | And(l, r) | Or(l, r) => {
                self.expr(l);
                self.expr(r);
            }
            Index(arr, ix) => {
                if matches!(arr, Var("FI")) {
                    self.header_columns.push(self.loc(e));
                }
                self.array(arr);
                self.expr(arr);
                self.expr(ix);
            }
            Assign(lhs, rhs) => {
                // `x = x + e` adds up like `x += e`.
                let sum = match (lhs, rhs) {
                    (Var(x), Binop(ast::Binop::Plus | ast::Binop::Minus, l, r)) => {
                        matches!(l, Var(y) if y == x) || matches!(r, Var(y) if y == x)
                    }
                    _ => false,
                };
                self.lvalue(lhs, if sum { SetKind::Sum } else { SetKind::Other });
                self.expr(rhs);
            }
            AssignOp(lhs, op, rhs) => {
                let kind = match op {
                    ast::Binop::Plus | ast::Binop::Minus => SetKind::Sum,
                    _ => SetKind::Other,
                };
                self.lvalue(lhs, kind);
                self.expr(rhs);
            }
            Inc { x, .. } => self.lvalue(x, SetKind::Sum),
            ITE(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            Getline { into, from, .. } => {
                if matches!(self.region, Region::Begin | Region::Function) {
                    self.getline = true;
                }
                if let Some(into) = into {
                    self.lvalue(into, SetKind::Other);
                }
                if let Some(from) = from {
                    self.expr(from);
                }
            }
            Call(f, args) => {
                let read_only = match f {
                    Either::Left(name) => {
                        self.calls.push((name, args.len(), self.loc(e)));
                        if *name == "sprintf" {
                            if let Some(StrLit(fmt)) = args.first() {
                                self.formats.push((fmt, args.len() - 1, self.loc(e)));
                            }
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
                    match arg {
                        Var(name) if !read_only => self.set(name, SetKind::Indirect, self.loc(arg)),
                        _ => self.expr(arg),
                    }
                }
            }
        }
    }
}

/// The number of arguments a `printf` format uses, or `None` if it uses positional arguments
/// (`%1$s`), which are not checked.
fn format_arguments(fmt: &[u8]) -> Option<usize> {
    let mut n = 0;
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
        while i < fmt.len() && b"-+ #0'".contains(&fmt[i]) {
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
                n += 1;
                i += 1;
            } else {
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    i += 1;
                }
                if fmt.get(i) == Some(&b'$') {
                    return None;
                }
            }
        }
        while i < fmt.len() && b"hlLqjzt".contains(&fmt[i]) {
            i += 1;
        }
        if i < fmt.len() {
            n += 1;
            i += 1;
        }
    }
    Some(n)
}

fn plural(n: usize, noun: &str) -> String {
    format!("{} {}{}", n, noun, if n == 1 { "" } else { "s" })
}

fn warning(code: &'static str, message: String, loc: Option<Loc>) -> Diagnostic {
    Diagnostic {
        code,
        severity: Severity::Warning,
        message,
        loc,
    }
}

/// Checks the syntax tree for mistakes that a run accepts silently.
fn check_ast<'a>(prog: &ast::Prog<'a, 'a, &'a str>, opts: &Options) -> Vec<Diagnostic> {
    let mut facts = Facts {
        locs: &prog.locs,
        region: Region::Begin,
        locals: &[],
        uses: Vec::new(),
        sets: Vec::new(),
        arrays: Vec::new(),
        calls: Vec::new(),
        formats: Vec::new(),
        read_only: Vec::new(),
        header_columns: Vec::new(),
        begin_record_reads: Vec::new(),
        getline: false,
    };
    prog.begin.iter().for_each(|s| facts.stmt(s));
    facts.region = Region::Main;
    for (pat, body) in prog.pats.iter() {
        match pat {
            ast::Pattern::Null => {}
            ast::Pattern::Bool(e) => facts.expr(e),
            ast::Pattern::Comma(l, r) => {
                facts.expr(l);
                facts.expr(r);
            }
        }
        if let Some(body) = body {
            facts.stmt(body);
        }
    }
    let worker_blocks = prog.beginfile.iter().chain(prog.endfile.iter()).chain(prog.prepare.iter());
    worker_blocks.for_each(|s| facts.stmt(s));
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
        let first = facts.header_columns.iter().flatten().min_by_key(|l| (l.line, l.col)).copied();
        diags.push(warning(
            "W105",
            "`FI` is only filled from the header line with `-H` (or `-i jsonl|parquet|markdown`): without it `FI` is empty and `$FI[\"col\"]` is always \"\"; run with `-H` (and pass `-H` to lint)".to_string(),
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

    // Variables that are read but never set anywhere: almost always a typo.
    let mut reported: Vec<&str> = Vec::new();
    for u in &facts.uses {
        let special = crate::builtins::VARIABLES.contains_key(u.name)
            || u.name == "SUBSEP"
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

    // printf formats that use more arguments than given: the missing ones print as empty.
    for (fmt, given, loc) in &facts.formats {
        if let Some(wanted) = format_arguments(fmt) {
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
    }

    // Calls of user functions with more arguments than parameters.
    for (name, given, loc) in &facts.calls {
        if let Some(dec) = prog.decs.iter().find(|d| d.name == *name) {
            if *given > dec.args.len() {
                diags.push(warning(
                    "W204",
                    format!(
                        "function `{}` is called with {} but declares {}: the extra arguments are ignored",
                        name,
                        plural(*given, "argument"),
                        plural(dec.args.len(), "parameter")
                    ),
                    *loc,
                ));
            }
        }
    }

    if let Some(mode) = opts.parallel {
        diags.extend(check_parallel(&facts, mode));
    }
    diags
}

/// Code whose meaning changes when the main loop runs in several workers (see
/// `info/parallelism.md`).
fn check_parallel(facts: &Facts, mode: Parallel) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    // Record counters count the records of one worker.
    let counters: &[&str] = match mode {
        Parallel::Record => &["NR", "FNR"],
        Parallel::File => &["NR"],
    };
    for counter in counters {
        if let Some(u) = facts.uses.iter().find(|u| u.region == Region::Main && u.name == *counter) {
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
            || !facts.uses.iter().any(|u| u.region == Region::End && u.name == s.name)
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
    diags
}

/// gawk special variables that zawk treats as ordinary variables.
const IGNORED_GAWK_VARIABLES: &[(&str, &str)] = &[
    ("FIELDWIDTHS", "fixed-width field splitting is not supported; use substr() or FS"),
    ("FPAT", "field splitting by content is not supported; use FS or match()/split()"),
    ("RT", "the text matched by RS is not available"),
    ("ERRNO", "system error messages are not stored in ERRNO"),
    ("BINMODE", "binary mode is not supported"),
    ("LINT", "dynamic lint checking is not supported; use `zawk lint`"),
    ("TEXTDOMAIN", "internationalization is not supported"),
    ("SYMTAB", "indirect variable access is not supported"),
    ("FUNCTAB", "indirect function calls are not supported"),
];

/// Reports what the token stream shows on its own: gawk variables zawk ignores and `f` used as a
/// name. Lexer errors are left to the parser.
fn scan_tokens(prog: &str) -> Vec<Diagnostic> {
    let toks: Vec<_> = lexer::Tokenizer::new(prog).map_while(Result::ok).collect();
    let mut diags = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    // `f` as a name clashes with f-strings: `f"x"` is an f-string, not `f` followed by "x".
    let (mut f_var, mut f_fun) = (false, false);
    for (loc, tok, _) in &toks {
        let kind = match tok {
            Tok::Ident("f") if !f_var => {
                f_var = true;
                "variable"
            }
            Tok::FunDec("f") | Tok::CallStart("f") if !f_fun => {
                f_fun = true;
                "function"
            }
            _ => continue,
        };
        diags.push(warning(
            "W103",
            format!(
                "`f` should not be used as a {} name: it clashes with f-strings (`f\"...\"` is an f-string, not `f` followed by a string); rename it",
                kind
            ),
            Some(*loc),
        ));
    }
    for (loc, tok, _) in &toks {
        let Tok::Ident(name) = tok else { continue };
        if let Some((_, why)) = IGNORED_GAWK_VARIABLES.iter().find(|(n, _)| n == name) {
            if !reported.contains(name) {
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
        assert_eq!(codes(&run("@include \"x.awk\"")), ["E201"]);
    }

    #[test]
    fn compile_errors() {
        let v = run("BEGIN {\n  print foo(1)\n}");
        assert_eq!(codes(&v), ["E301"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        let v = run("BEGIN { patsplit(\"a\", b) }");
        assert_eq!(codes(&v), ["E203"]);
        assert!(v["diagnostics"][0]["message"].as_str().unwrap().contains("match()"));
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
        assert_eq!(v["diagnostics"][0]["message"], "function `substr` expects 2 or 3 arguments, got 1");
    }

    #[test]
    fn warnings() {
        let v = run("BEGIN { FIELDWIDTHS = \"2 3\" }");
        assert_eq!(v["valid"], true);
        assert_eq!(codes(&v), ["W101"]);
    }

    #[test]
    fn function_name_conflicts() {
        let v = run("function g(a, a) { return a }\nBEGIN { print g(1, 2) }");
        assert_eq!(codes(&v), ["E305"]);
        assert_eq!(codes(&run("function h(h) { return 1 }")), ["E305"]);
        assert_eq!(codes(&run("function h(x) { return x }\nfunction g(h) { return h }")), ["E305"]);
        let v = run("function foo(x) { return x }\nBEGIN {\n  foo = 1; print foo(2) }");
        assert_eq!(codes(&v), ["E305"]);
        assert_eq!(v["diagnostics"][0]["line"], 3);
        assert_eq!(codes(&run("function foo(x) { return x }\nBEGIN { foo[1] = 1 }")), ["E305"]);
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
        let header = Options { header: true, ..Options::default() };
        let v = run_with("{ $FI[\"name\"] = \"x\"; FI[\"name\"] = 1 }", &header);
        assert_eq!(codes(&v), ["E305", "E305"]);
        assert!(v["diagnostics"][0]["message"].as_str().unwrap().contains("$FI[...]"));
        // Reading them is fine.
        assert!(codes(&run_with("{ print ARGV[1], ARGC, FILENAME, NR, NF, $0, $1, $FI[\"name\"] }", &header)).is_empty());
    }

    #[test]
    fn f_name() {
        let v = run("function f(x) { return x }\nfunction g(y) { return f(y) }\nBEGIN { f = 1; print g(f) }");
        assert_eq!(codes(&v), ["W103", "W103", "E305"]);
        assert!(v["diagnostics"][0]["message"].as_str().unwrap().contains("function"));
        assert_eq!(v["diagnostics"][1]["line"], 3);
        assert_eq!(v["diagnostics"][1]["column"], 9);
        assert!(codes(&run("BEGIN { x = fx = xf = 1; print f\"{x}\", fx, xf }")).is_empty());
    }

    fn run_with(prog: &str, opts: &Options) -> Value {
        let src = Source {
            name: None,
            text: prog.to_string(),
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
        let opts = Options { assigned: vec!["n".into()], ..Options::default() };
        assert!(codes(&run_with("{ print n }", &opts)).is_empty());
        assert!(codes(&run("# @var n count\n# @var m? other\n{ print n, m }")).is_empty());
        // An array that is only read is empty too.
        assert_eq!(codes(&run("{ if ($1 in seen) print }")), ["W201"]);
    }

    #[test]
    fn format_arguments() {
        assert_eq!(super::format_arguments(b"%s %d%%\n"), Some(2));
        assert_eq!(super::format_arguments(b"%-*.*f|%5.2e %lld"), Some(5));
        assert_eq!(super::format_arguments(b"%2$s %1$s"), None);
        let v = run("BEGIN {\n  printf \"%s %s\\n\", \"a\"\n}");
        assert_eq!(codes(&v), ["W202"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 3);
        assert_eq!(codes(&run("BEGIN { x = sprintf(\"%d\", 1, 2) }")), ["W203"]);
        assert_eq!(codes(&run("BEGIN { printf(\"%s\") }")), ["W202"]);
        assert!(codes(&run("BEGIN { printf \"%s=%d\\n\", \"a\", 1; x = \"%s\"; printf x }")).is_empty());
    }

    #[test]
    fn header_columns() {
        let prog = "BEGIN { n = 1 }\n{ print $FI[\"name\"], FI[\"age\"] }";
        let v = run(prog);
        assert_eq!(codes(&v), ["W105"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 10);
        let header = Options { header: true, ..Options::default() };
        assert!(codes(&run_with(prog, &header)).is_empty());
    }

    #[test]
    fn begin_record_reads() {
        let v = run("BEGIN {\n  print $1, $1, NF, FILENAME, $(i)\n  i = 2\n}");
        assert_eq!(codes(&v), ["W218", "W218", "W218", "W218"]);
        assert_eq!(v["diagnostics"][0]["line"], 2);
        assert_eq!(v["diagnostics"][0]["column"], 9);
        assert!(v["diagnostics"][0]["message"].as_str().unwrap().starts_with("`$1`"));
        assert!(v["diagnostics"][1]["message"].as_str().unwrap().contains("always 0"));
        // After `getline`, or outside BEGIN, there is a record.
        assert!(codes(&run("BEGIN { getline; print $1, NF }")).is_empty());
        assert!(codes(&run("BEGIN { while ((getline < \"f\") > 0) print $0 }")).is_empty());
        assert!(codes(&run("function r() { return getline } BEGIN { r(); print $0 }")).is_empty());
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
        let pr = Options { parallel: Some(Parallel::Record), ..Options::default() };
        let v = run_with(prog, &pr);
        assert_eq!(codes(&v), ["W301", "W302"]);
        assert_eq!(v["diagnostics"][1]["line"], 2);
        assert_eq!(v["diagnostics"][1]["column"], 3);
        // Values collected per worker in PREPARE are the documented fix.
        let fixed = "{ max = max > $1 ? max : $1 } PREPARE { m[PID] = max } END { for (p in m) print m[p] }";
        assert!(codes(&run_with(fixed, &pr)).is_empty());
        let pf = Options { parallel: Some(Parallel::File), ..Options::default() };
        assert!(codes(&run_with("FNR == 1 { print FILENAME }", &pf)).is_empty());
        assert_eq!(codes(&run_with("FNR == 1 { print FILENAME }", &pr)), ["W301"]);
    }

    #[test]
    fn multiple_sources() {
        let srcs = [
            Source {
                name: Some("a.awk".into()),
                text: "function g(x) { return x }".into(),
            },
            Source {
                name: Some("b.awk".into()),
                text: "BEGIN {\n  print g(1\n}".into(),
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
            },
            Source {
                name: Some(parsing::inline_source_name(1)),
                text: "BEGIN {\n  print 1 +\n}".into(),
            },
        ];
        let v: Value = serde_json::from_str(&lint(&srcs, &Options::default()).2).unwrap();
        let d = &v["diagnostics"][0];
        assert_eq!(d["file"], "-e#1");
        assert_eq!(d["line"], 2);
    }
}
