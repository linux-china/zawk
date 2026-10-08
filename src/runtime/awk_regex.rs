//! Compiling awk (POSIX ERE, with gawk's extensions) regular expressions with the `regex` crate.
//!
//! The syntaxes mostly agree; `translate` rewrites the parts that do not:
//!
//! * `.` matches any character, including a newline (set on the builder, see `compile`).
//! * A `{` that does not start a valid interval (`{n}`, `{n,}`, `{n,m}`) is a literal, as is one
//!   at the start of an expression.
//! * gawk's operators: `\y` (word boundary), `\<` and `\>` (start/end of a word), `` \` `` and
//!   `\'` (start/end of the string), and `\B`.
//! * Escapes: `\b` is a backspace (gawk uses `\y` for word boundaries), `\a`, `\v`, `\f` and
//!   octal escapes (`\101`); other letters that are not operators are literal characters.
//! * In bracket expressions, `[`, `&`, `~` and `-` sequences that the `regex` crate treats as
//!   nested classes or set operations are literal characters.
//!
//! One difference remains: `regex` reports the leftmost-first match of an alternation (e.g. `x`
//! for `x|xy` against "xyz"), where POSIX awk reports the leftmost-longest one (`xy`).
use regex::bytes::{Regex, RegexBuilder};

/// Compile an awk regular expression.
pub(crate) fn compile(pat: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&translate(pat))
        .dot_matches_new_line(true)
        .build()
}

fn push_literal(out: &mut String, c: char) {
    if c.is_ascii_punctuation() {
        out.push('\\');
    }
    out.push(c);
}

fn push_octal(out: &mut String, chars: &[char], i: &mut usize) {
    let mut value = 0u32;
    let mut n = 0;
    while n < 3 && *i < chars.len() && ('0'..='7').contains(&chars[*i]) {
        value = value * 8 + chars[*i].to_digit(8).unwrap();
        *i += 1;
        n += 1;
    }
    out.push_str(&format!("\\x{{{:x}}}", value & 0xff));
}

/// Translate the escape sequence at `chars[*i]` (just after a backslash).
fn translate_escape(out: &mut String, chars: &[char], i: &mut usize, in_bracket: bool) {
    let c = match chars.get(*i) {
        Some(c) => *c,
        None => {
            // A trailing backslash is a literal backslash.
            out.push_str("\\\\");
            return;
        }
    };
    *i += 1;
    match c {
        'y' if !in_bracket => out.push_str(r"\b"),
        '<' if !in_bracket => out.push_str(r"\b{start}"),
        '>' if !in_bracket => out.push_str(r"\b{end}"),
        '`' if !in_bracket => out.push_str(r"\A"),
        '\'' if !in_bracket => out.push_str(r"\z"),
        'B' if !in_bracket => out.push_str(r"\B"),
        'b' => out.push_str(r"\x08"),
        'a' => out.push_str(r"\x07"),
        'v' => out.push_str(r"\x0B"),
        'f' => out.push_str(r"\x0C"),
        'n' | 't' | 'r' | 'd' | 'D' | 's' | 'S' | 'w' | 'W' => {
            out.push('\\');
            out.push(c);
        }
        // Unicode classes and hex escapes, e.g. \p{Greek} or \x41, as supported by `regex`.
        'p' | 'P' | 'x' => {
            out.push('\\');
            out.push(c);
        }
        '0'..='7' => {
            *i -= 1;
            push_octal(out, chars, i);
        }
        // Other escaped characters are literal (gawk warns about unknown escapes).
        c => push_literal(out, c),
    }
}

/// Whether `chars[i..]` starts with a valid interval: `{n}`, `{n,}` or `{n,m}`.
fn interval_len(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    let start = j;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if j == start {
        return None;
    }
    if chars.get(j) == Some(&',') {
        j += 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
    }
    if chars.get(j) == Some(&'}') {
        Some(j + 1 - i)
    } else {
        None
    }
}

/// Translate the bracket expression starting at `chars[*i]` (a `[`). Returns false if it is not
/// terminated, in which case nothing is consumed.
fn translate_bracket(out: &mut String, chars: &[char], i: &mut usize) -> bool {
    let mut j = *i + 1;
    let mut res = String::from("[");
    if chars.get(j) == Some(&'^') {
        res.push('^');
        j += 1;
    }
    // A `]` first in the list is a literal.
    if chars.get(j) == Some(&']') {
        res.push_str(r"\]");
        j += 1;
    }
    loop {
        let c = match chars.get(j) {
            Some(c) => *c,
            None => return false,
        };
        match c {
            ']' => {
                res.push(']');
                j += 1;
                break;
            }
            '[' => match chars.get(j + 1) {
                // Character classes like [:alpha:].
                Some(':') => {
                    let end = (j + 2..chars.len().saturating_sub(1))
                        .find(|&k| chars[k] == ':' && chars[k + 1] == ']');
                    match end {
                        Some(end) => {
                            res.extend(&chars[j..end + 2]);
                            j = end + 2;
                        }
                        None => {
                            res.push_str(r"\[");
                            j += 1;
                        }
                    }
                }
                // Collating symbols and equivalence classes of one character, e.g. [.-.].
                Some('.') | Some('=') if chars.get(j + 3) == Some(&chars[j + 1]) && chars.get(j + 4) == Some(&']') => {
                    push_literal(&mut res, chars[j + 2]);
                    j += 5;
                }
                _ => {
                    res.push_str(r"\[");
                    j += 1;
                }
            },
            '\\' => {
                j += 1;
                translate_escape(&mut res, chars, &mut j, /*in_bracket=*/ true);
            }
            // Set operators in the `regex` crate; literal characters in awk.
            '&' | '~' => {
                res.push('\\');
                res.push(c);
                j += 1;
            }
            '-' if chars.get(j + 1) == Some(&'-') => {
                res.push_str(r"\-");
                j += 1;
            }
            c => {
                res.push(c);
                j += 1;
            }
        }
    }
    out.push_str(&res);
    *i = j;
    true
}

/// Translate an awk regular expression into the syntax of the `regex` crate.
pub(crate) fn translate(pat: &str) -> String {
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::with_capacity(pat.len() + 8);
    // Whether we are at the start of an expression (where a repetition operator is a literal).
    let mut at_start = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                i += 1;
                translate_escape(&mut out, &chars, &mut i, /*in_bracket=*/ false);
                at_start = false;
            }
            '[' => {
                if !translate_bracket(&mut out, &chars, &mut i) {
                    out.push_str(r"\[");
                    i += 1;
                }
                at_start = false;
            }
            '{' => {
                match interval_len(&chars, i) {
                    Some(len) if !at_start => {
                        out.extend(&chars[i..i + len]);
                        i += len;
                    }
                    _ => {
                        out.push_str(r"\{");
                        i += 1;
                    }
                }
                at_start = false;
            }
            '}' => {
                out.push_str(r"\}");
                i += 1;
                at_start = false;
            }
            '(' => {
                out.push('(');
                i += 1;
                // Pass through groups like `(?:...)` (zawk generates them; gawk has none).
                if chars.get(i) == Some(&'?') {
                    out.push('?');
                    i += 1;
                }
                at_start = true;
            }
            '|' => {
                out.push('|');
                i += 1;
                at_start = true;
            }
            '^' => {
                out.push('^');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
                at_start = false;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_match(pat: &str, s: &str) -> bool {
        compile(pat).unwrap().is_match(s.as_bytes())
    }

    #[test]
    fn translation() {
        assert_eq!(translate(r"\yfoo\y"), r"\bfoo\b");
        assert_eq!(translate(r"\<a\>"), r"\b{start}a\b{end}");
        assert_eq!(translate("a{"), r"a\{");
        assert_eq!(translate("{"), r"\{");
        assert_eq!(translate("a{,}"), r"a\{,\}");
        assert_eq!(translate("a{2}b{1,}c{1,3}"), "a{2}b{1,}c{1,3}");
        assert_eq!(translate(r"\101"), r"\x{41}");
        assert_eq!(translate("[[]"), r"[\[]");
        assert_eq!(translate("[]a]"), r"[\]a]");
        assert_eq!(translate("[^]a]"), r"[^\]a]");
        assert_eq!(translate("[[:alpha:]_]"), "[[:alpha:]_]");
        assert_eq!(translate("[a&&b]"), r"[a\&\&b]");
        assert_eq!(translate("(?:.)"), "(?:.)");
    }

    #[test]
    fn matching() {
        assert!(is_match("a.b", "a\nb"));
        assert!(is_match("{", "{"));
        assert!(is_match("a{", "xa{"));
        assert!(is_match("a{,}", "a{,}"));
        assert!(!is_match("^a{2}$", "a"));
        assert!(is_match(r"\yfoo\y", "a foo b"));
        assert!(!is_match(r"\yfoo\y", "afoob"));
        assert!(is_match(r"\<foo\>", "foo"));
        assert!(!is_match(r"\<oo", "foo"));
        assert!(is_match(r"\`ab\'", "ab"));
        assert!(!is_match(r"\`b", "ab"));
        assert!(is_match("[[]", "["));
        assert!(is_match("[]]", "]"));
        assert!(is_match("[a&]", "&"));
        assert!(is_match(r"\101", "A"));
        assert!(is_match(r"\q", "q"));
        assert!(is_match("[[:digit:]]+", "x12"));
    }
}
