use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub syntax);

use crate::ast::{Expr, Stmt};
use crate::common::{fatal_message, CompileError, FATAL_PREFIX};
use crate::lexer::{self, Loc, Tok};
use lalrpop_util::ParseError;

/// A case of a switch statement: the location of its label, the label (`None` for `default`) and
/// its body.
pub(crate) type SwitchCase<'a> = (
    Loc,
    Option<&'a Expr<'a, 'a, &'a str>>,
    &'a Stmt<'a, 'a, &'a str>,
);

/// Rejects a switch body with more than one `default`, or with two labels with the same string
/// value (`1`, `1.0` and `"1"` are duplicates; regex labels are not checked), as gawk does.
pub(crate) fn check_switch_cases<'a>(
    cases: &[SwitchCase<'a>],
) -> Result<(), ParseError<Loc, Tok<'a>, lexer::Error>> {
    let error = |location: Loc, desc: String| {
        Err(ParseError::User {
            error: lexer::Error {
                location,
                desc: desc.into(),
            },
        })
    };
    let mut seen = hashbrown::HashSet::<Vec<u8>>::default();
    let mut has_default = false;
    for (loc, label, _) in cases {
        let key = match label {
            None if has_default => {
                return error(*loc, "duplicate `default' detected in switch body".into());
            }
            None => {
                has_default = true;
                continue;
            }
            Some(Expr::ILit(n)) => n.to_string().into_bytes(),
            Some(Expr::FLit(f)) => crate::runtime::numfmt::with_number_bytes(*f, false, |b| b.to_vec()),
            Some(Expr::StrLit(s)) => s.to_vec(),
            Some(_) => continue,
        };
        if !seen.insert(key.clone()) {
            return error(
                *loc,
                format!(
                    "duplicate case values in switch body: {}",
                    String::from_utf8_lossy(&key)
                ),
            );
        }
    }
    Ok(())
}

/// The name of the `n`th (1-based) `-e` source in messages, when the program has other sources:
/// `-e#2`, so that `-e#2:1:7` is not taken for line 1 of another source.
pub fn inline_source_name(n: usize) -> String {
    format!("-e#{}", n)
}

/// The parts of the program text: `-f` files, `-e` strings or the program operand, concatenated
/// with a newline after each, as `main` does. Used to report locations as `file:line:column`.
#[derive(Debug, Default, Clone)]
pub struct SourceMap {
    /// The file name (`None` for a program given on the command line) and the line of the
    /// program text each part starts at.
    parts: Vec<(Option<String>, usize)>,
}

impl SourceMap {
    /// Adds a part starting at line `start_line` (0-based) of the program text.
    pub fn push(&mut self, name: Option<String>, start_line: usize) {
        self.parts.push((name, start_line));
    }

    /// `loc` in `prog` for messages: `file.awk:3:5` in a file, `line 3, column 5` otherwise.
    /// Lines count from the start of the part, columns count characters; both start at 1.
    pub fn describe(&self, prog: &str, loc: Loc) -> String {
        let (name, start) = self
            .parts
            .iter()
            .rev()
            .find(|(_, start)| *start <= loc.line)
            .map_or((None, 0), |(name, start)| (name.as_deref(), *start));
        let line = loc.line - start + 1;
        let col = match prog.lines().nth(loc.line) {
            Some(text) => text.get(..loc.col).unwrap_or(text).chars().count() + 1,
            None => loc.col + 1,
        };
        match name {
            Some(name) => format!("{}:{}:{}", name, line, col),
            None => format!("line {}, column {}", line, col),
        }
    }
}

/// The program text and its parts, for error messages raised after parsing (see `render_error`).
static PROGRAM: std::sync::OnceLock<(String, SourceMap)> = std::sync::OnceLock::new();

/// Registers the program being run, so that errors can point into it.
pub fn set_program(text: &str, sources: SourceMap) {
    let _ = PROGRAM.set((text.to_string(), sources));
}

fn program_sources() -> Option<&'static SourceMap> {
    PROGRAM.get().map(|(_, sources)| sources)
}

/// Formats a parse error in the gawk style: the location, what went wrong, then the line of the
/// program with a `^` under the error.
///
/// ```text
/// zawk: syntax error at line 1, column 19: unexpected `}`
///     BEGIN { if (1) { }}}
///                       ^
/// ```
pub fn syntax_error(prog: &str, err: &ParseError<Loc, Tok, lexer::Error>) -> String {
    let (loc, desc) = describe_parse_error(prog, err);
    let at = program_sources().map_or_else(
        || SourceMap::default().describe(prog, loc),
        |sources| sources.describe(prog, loc),
    );
    format!("zawk: syntax error at {}: {}{}", at, desc, excerpt(prog, loc))
}

/// The location of a parse error and what went wrong.
pub fn describe_parse_error(prog: &str, err: &ParseError<Loc, Tok, lexer::Error>) -> (Loc, String) {
    match err {
        ParseError::InvalidToken { location } => (*location, "invalid token".to_string()),
        ParseError::UnrecognizedEof { location, expected } => (
            *location,
            format!("unexpected end of program{}", expected_tokens(expected)),
        ),
        ParseError::UnrecognizedToken {
            token: (l, _, r),
            expected,
        } => (
            *l,
            format!("unexpected {}{}", token_text(prog, l, r), expected_tokens(expected)),
        ),
        ParseError::ExtraToken { token: (l, _, r) } => {
            (*l, format!("unexpected {}", token_text(prog, l, r)))
        }
        ParseError::User { error } => (error.location, error.desc.to_string()),
    }
}

/// The line of `prog` at `loc` with a `^` under `loc`, each on its own indented line.
fn excerpt(prog: &str, loc: Loc) -> String {
    match prog.lines().nth(loc.line) {
        Some(line) => {
            // Keep tabs, so that the `^` lines up with the line above.
            let indent: String = line
                .get(..loc.col)
                .unwrap_or(line)
                .chars()
                .map(|c| if c == '\t' { '\t' } else { ' ' })
                .collect();
            format!("\n    {}\n    {}^", line, indent)
        }
        None => String::new(),
    }
}

/// Formats an error of a compiled or running program in the gawk style. An error with a location
/// in the program registered with `set_program` (such as an unknown function, or a call with the
/// wrong number of arguments) is reported like a syntax error, with the location and the line of
/// the program:
///
/// ```text
/// zawk: fatal: prog.awk:3:5: function `substr` expects 2 or 3 arguments, got 1
///         x = substr(s)
///             ^
/// ```
pub fn render_error(err: &CompileError) -> String {
    match (err.1, PROGRAM.get()) {
        (Some(loc), Some((prog, sources))) => {
            let msg = err.0.strip_prefix(FATAL_PREFIX).unwrap_or(&err.0);
            format!(
                "zawk: {}{}: {}{}",
                FATAL_PREFIX,
                sources.describe(prog, loc),
                msg,
                excerpt(prog, loc)
            )
        }
        _ => fatal_message(&err.0),
    }
}

/// The source text of a token, as it appears in the program.
fn token_text(prog: &str, l: &Loc, r: &Loc) -> String {
    let text = prog.get(l.offset()..r.offset()).unwrap_or("").trim();
    // A `;` the lexer inserts before a `}` is empty: show the character at its location.
    let text = if text.is_empty() {
        prog.get(l.offset()..).and_then(|s| s.chars().next()).map_or(String::new(), String::from)
    } else {
        text.to_string()
    };
    match text.as_str() {
        "" => "end of program".to_string(),
        "\n" => "newline".to_string(),
        _ => format!("`{}`", text),
    }
}

/// ", expected ..." for a short list of expected tokens; long lists only add noise.
fn expected_tokens(expected: &[String]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for tok in expected {
        let name = match tok.trim_matches('"') {
            "INT" | "HEX" | "FLOAT" => "number",
            "IDENT" => "identifier",
            "STRLIT" => "string",
            "PATLIT" => "regex",
            "CALLSTART" => "function call",
            "FUNDEC" => "function",
            "\\n" => "newline",
            other => other,
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    match names.len() {
        0 => String::new(),
        1..=4 => format!(
            ", expected {}",
            names.iter().map(|n| format!("`{}`", n)).collect::<Vec<_>>().join(" or ")
        ),
        _ => String::new(),
    }
}
