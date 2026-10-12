use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub syntax);

use crate::ast::{Expr, Stmt};
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

/// Formats a parse error in the gawk style: the location, what went wrong, then the line of the
/// program with a `^` under the error.
///
/// ```text
/// zawk: syntax error at line 1, column 19: unexpected `}`
///     BEGIN { if (1) { }}}
///                       ^
/// ```
pub fn syntax_error(prog: &str, err: &ParseError<Loc, Tok, lexer::Error>) -> String {
    let (loc, desc) = match err {
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
    };
    let mut msg = format!("zawk: syntax error at {}: {}", loc, desc);
    if let Some(line) = prog.lines().nth(loc.line) {
        // Keep tabs, so that the `^` lines up with the line above.
        let indent: String = line
            .get(..loc.col)
            .unwrap_or(line)
            .chars()
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect();
        msg.push_str(&format!("\n    {}\n    {}^", line, indent));
    }
    msg
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
