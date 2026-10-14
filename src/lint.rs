//! `zawk lint`: checks a program without running it and reports the problems found as JSON.
//!
//! The program goes through the same stages as a real run (lexing, parsing, building the CFG and
//! type checking), but every error becomes a diagnostic with a code, a location and the line of
//! source it points at, instead of a message on stderr. A token scan also reports gawk features
//! that zawk accepts but ignores (e.g. `FIELDWIDTHS`), which a run would silently get wrong.
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

/// Lints `sources` as one program (they are concatenated, as for a run) and returns the JSON
/// report with whether the program is free of errors.
pub fn lint(sources: &[Source], allow_arbitrary_commands: bool) -> (bool, String) {
    // Each source is followed by a newline, as `main` does when it concatenates them.
    let mut prog = String::new();
    let mut starts = Vec::with_capacity(sources.len());
    for src in sources {
        starts.push(prog.matches('\n').count());
        prog.push_str(&src.text);
        prog.push('\n');
    }
    let mut diags = scan_tokens(&prog);
    if let Some(d) = check_program(&prog, allow_arbitrary_commands) {
        diags.push(d);
    }
    diags.sort_by_key(|d| d.loc.map_or((usize::MAX, 0), |l| (l.line, l.col)));

    let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
    let lines: Vec<&str> = prog.lines().collect();
    let items: Vec<Object> = diags
        .iter()
        .map(|d| to_json(d, &lines, sources, &starts))
        .collect();
    (errors == 0, report(errors, diags.len() - errors, &items))
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

/// Parses, builds and type checks the program, returning the first error found.
fn check_program(prog: &str, allow_arbitrary_commands: bool) -> Option<Diagnostic> {
    let a = Arena::default();
    let text: &str = a.alloc_str(prog);
    let mut buf = Vec::new();
    let mut program = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    if let Err(e) = parser.parse(&a, &mut buf, &mut program, lexer::Tokenizer::new(text)) {
        return Some(parse_diagnostic(text, &e));
    }
    let program = a.alloc(program);
    // The type checker reports some unsupported programs with a panic: report it as an error
    // instead of crashing (and keep the panic message off stderr).
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ctx = cfg::ProgramContext::from_prog(&a, program, Escaper::Identity)?;
        ctx.allow_arbitrary_commands = allow_arbitrary_commands;
        compile::context_compiles(&mut ctx)
    }));
    std::panic::set_hook(hook);
    match res {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(compile_diagnostic(text, &e.0)),
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
    let msg = parsing::syntax_error(prog, err);
    let msg = msg.lines().next().unwrap_or("");
    let message = match msg.split_once(&format!("{}: ", loc)) {
        Some((_, desc)) => desc.to_string(),
        None => msg.trim_start_matches("zawk: ").to_string(),
    };
    Diagnostic {
        code,
        severity: Severity::Error,
        message,
        loc: Some(loc),
    }
}

/// gawk builtin functions; calling one that zawk lacks is an unsupported feature rather than a
/// typo.
const GAWK_FUNCTIONS: &[&str] = &[
    "patsplit", "isarray", "asort", "asorti", "mkbool", "bindtextdomain", "dcgettext",
    "dcngettext", "compl", "lshift", "rshift", "and", "or", "xor", "strtonum", "systime",
    "mktime", "strftime", "gensub", "typeof", "fflush", "close",
];

fn compile_diagnostic(prog: &str, msg: &str) -> Diagnostic {
    let msg = msg.trim_start_matches("zawk: ").trim_start_matches("fatal: ");
    let name = quoted_name(msg);
    let (code, call) = if msg.starts_with("Call to unknown function") {
        match name {
            Some(n) if GAWK_FUNCTIONS.contains(&n) => ("E203", true),
            _ => ("E301", true),
        }
    } else if msg.contains("arrays of arrays") {
        ("E202", false)
    } else if msg.contains(" inputs but got ") {
        ("E302", true)
    } else if msg.contains("as an array") || msg.contains("scalar context") {
        ("E303", false)
    } else if msg.contains("user input") || msg.contains("arbitrary") {
        ("E304", false)
    } else {
        ("E300", false)
    };
    let message = if code == "E203" {
        format!("{}: not supported by zawk", msg)
    } else {
        msg.to_string()
    };
    // Compile errors carry no location: point at the first use of the name they mention.
    let name = if code == "E302" {
        msg.strip_prefix("function ").and_then(|s| s.split(' ').next())
    } else {
        name
    };
    let loc = name.and_then(|n| find_name(prog, n, call));
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

/// Reports what the token stream shows on its own: gawk variables zawk ignores and assignments
/// to the read-only `ARGV` and `ARGC`. Lexer errors are left to the parser.
fn scan_tokens(prog: &str) -> Vec<Diagnostic> {
    let toks: Vec<_> = lexer::Tokenizer::new(prog).map_while(Result::ok).collect();
    let mut diags = Vec::new();
    let mut reported: Vec<&str> = Vec::new();
    for (i, (loc, tok, _)) in toks.iter().enumerate() {
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
        } else if matches!(*name, "ARGV" | "ARGC") {
            // Skip `[...]` after ARGV to find the token following the lvalue.
            let mut j = i + 1;
            if toks.get(j).map(|t| &t.1) == Some(&Tok::LBrack) {
                let mut depth = 0;
                while let Some((_, t, _)) = toks.get(j) {
                    match t {
                        Tok::LBrack => depth += 1,
                        Tok::RBrack => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                j += 1;
            }
            let assigned = matches!(
                toks.get(j).map(|t| &t.1),
                Some(
                    Tok::Assign
                        | Tok::AddAssign
                        | Tok::SubAssign
                        | Tok::MulAssign
                        | Tok::DivAssign
                        | Tok::PowAssign
                        | Tok::ModAssign
                        | Tok::Incr
                        | Tok::Decr
                )
            ) || matches!(toks.get(i.wrapping_sub(1)).map(|t| &t.1), Some(Tok::Delete));
            if assigned {
                diags.push(Diagnostic {
                    code: "W102",
                    severity: Severity::Warning,
                    message: format!(
                        "`{}` is read-only in zawk: changing it does not change the input files",
                        name
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
        serde_json::from_str(&lint(&[src], false).1).unwrap()
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
        assert_eq!(codes(&run("BEGIN { patsplit(\"a\", b) }")), ["E203"]);
        assert_eq!(codes(&run("BEGIN { a[1][2] = 3 }")), ["E202"]);
        assert_eq!(codes(&run("BEGIN { print length(1, 2) }")), ["E302"]);
        assert_eq!(codes(&run("BEGIN { x = 1; x[1] = 2 }")), ["E303"]);
    }

    #[test]
    fn warnings() {
        let v = run("BEGIN { FIELDWIDTHS = \"2 3\"; ARGV[1] = \"x\"; ARGC++ }");
        assert_eq!(v["valid"], true);
        assert_eq!(codes(&v), ["W101", "W102", "W102"]);
        assert!(codes(&run("BEGIN { print ARGV[1], ARGC }")).is_empty());
    }

    #[test]
    fn multiple_sources() {
        let srcs = [
            Source {
                name: Some("a.awk".into()),
                text: "function f(x) { return x }".into(),
            },
            Source {
                name: Some("b.awk".into()),
                text: "BEGIN {\n  print f(1\n}".into(),
            },
        ];
        let v: Value = serde_json::from_str(&lint(&srcs, false).1).unwrap();
        let d = &v["diagnostics"][0];
        assert_eq!(d["file"], "b.awk");
        assert_eq!(d["line"], 2);
    }
}
