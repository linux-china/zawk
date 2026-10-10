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
//! * POSIX character classes (`[:alpha:]` etc.) match non-ASCII characters too, as in gawk under a
//!   UTF-8 locale (the `regex` crate's own versions are ASCII-only); see `posix_class`.
//! * In bracket expressions, `[`, `&`, `~` and `-` sequences that the `regex` crate treats as
//!   nested classes or set operations are literal characters.
//!
//! `regex` reports the leftmost-first match of an alternation (e.g. `x` for `x|xy` against
//! "xyz"), where POSIX awk reports the leftmost-longest one (`xy`). The `find_at`, `find_iter`
//! and `captures_at` helpers below restore the POSIX semantics for patterns with an alternation;
//! other patterns use `regex` directly.
use regex::bytes::{Captures, Regex, RegexBuilder};
use regex_automata::hybrid::dfa::{Cache, DFA};
use regex_automata::{Anchored, Input, MatchKind};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Compile an awk regular expression.
pub(crate) fn compile(pat: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&translate(pat))
        .dot_matches_new_line(true)
        .build()
}

/// The syntax error in `pat`, if any, e.g. "unclosed group".
pub(crate) fn syntax_error(pat: &str) -> Option<String> {
    regex_syntax::ast::parse::Parser::new()
        .parse(&translate(pat))
        .err()
        .map(|e| e.kind().to_string())
}

/// A one-line message for an error compiling `pat`: `invalid regex /(/: unclosed group`.
pub(crate) fn compile_error(pat: &str, err: &regex::Error) -> String {
    let reason = syntax_error(pat).unwrap_or_else(|| match err {
        regex::Error::CompiledTooBig(_) => "regex too big".to_string(),
        _ => err.to_string(),
    });
    format!("invalid regex /{}/: {}", pat, reason)
}

/// What it takes to turn a leftmost-first match into the leftmost-longest one.
struct Longest {
    /// A lazy DFA reporting all matches, so that an anchored forward search ends at the longest.
    dfa: DFA,
    cache: RefCell<Cache>,
    /// The pattern anchored at the end, `(?:pat)\z`, to get capture groups of a given span.
    exact: OnceCell<Option<Regex>>,
}

thread_local! {
    static LONGEST: RefCell<HashMap<String, Option<Rc<Longest>>>> = RefCell::new(HashMap::new());
}

/// The longest-match machinery for `re`, or `None` when leftmost-first is already leftmost-longest
/// (no alternation) or the DFA cannot be built.
fn longest(re: &Regex) -> Option<Rc<Longest>> {
    let pat = re.as_str();
    if !pat.contains('|') {
        return None;
    }
    LONGEST.with(|cell| {
        if let Some(l) = cell.borrow().get(pat) {
            return l.clone();
        }
        let dfa = DFA::builder()
            .configure(DFA::config().match_kind(MatchKind::All))
            .syntax(
                regex_automata::util::syntax::Config::new()
                    .utf8(false)
                    .dot_matches_new_line(true),
            )
            .thompson(regex_automata::nfa::thompson::Config::new().utf8(false))
            .build(pat)
            .ok();
        let l = dfa.map(|dfa| {
            let cache = RefCell::new(dfa.create_cache());
            Rc::new(Longest {
                dfa,
                cache,
                exact: OnceCell::new(),
            })
        });
        cell.borrow_mut().insert(pat.to_string(), l.clone());
        l
    })
}

/// The end of the longest match of `l` starting at `start`.
fn longest_end(l: &Longest, hay: &[u8], start: usize) -> Option<usize> {
    let input = Input::new(hay).range(start..).anchored(Anchored::Yes);
    let mut cache = l.cache.borrow_mut();
    // The lazy DFA gives up on e.g. Unicode word boundaries next to non-ASCII text; the caller
    // then keeps the leftmost-first match.
    l.dfa.try_search_fwd(&mut cache, &input).ok().flatten().map(|m| m.offset())
}

/// The leftmost-longest match of `re` in `hay` starting at or after `start`, as (start, end).
pub(crate) fn find_at(re: &Regex, hay: &[u8], start: usize) -> Option<(usize, usize)> {
    let m = re.find_at(hay, start)?;
    let (from, to) = (m.start(), m.end());
    match longest(re) {
        Some(l) => Some((from, longest_end(&l, hay, from).map_or(to, |e| e.max(to)))),
        None => Some((from, to)),
    }
}

/// All successive non-overlapping leftmost-longest matches of `re` in `hay`, as (start, end).
/// As with `Regex::find_iter`, an empty match right after the previous match is skipped.
pub(crate) fn find_iter<'h>(
    re: &'h Regex,
    hay: &'h [u8],
) -> impl Iterator<Item = (usize, usize)> + 'h {
    let mut pos = 0;
    let mut last_end = None;
    std::iter::from_fn(move || loop {
        if pos > hay.len() {
            return None;
        }
        let (from, to) = find_at(re, hay, pos)?;
        if from == to && last_end == Some(to) {
            pos = to + 1;
            continue;
        }
        pos = to;
        last_end = Some(to);
        return Some((from, to));
    })
}

/// The capture groups of `re` for the match `from..to` of `hay` (as found by `find_at`).
pub(crate) fn captures_at<'h>(re: &Regex, hay: &'h [u8], from: usize, to: usize) -> Option<Captures<'h>> {
    if let Some(l) = longest(re) {
        let exact = l.exact.get_or_init(|| {
            RegexBuilder::new(&format!("(?:{})\\z", re.as_str()))
                .dot_matches_new_line(true)
                .build()
                .ok()
        });
        if let Some(exact) = exact {
            // Text before `from` stays visible to look-behind assertions such as `\b`.
            if let Some(c) = exact.captures_at(&hay[..to], from) {
                if c.get(0).map(|m| m.start()) == Some(from) {
                    return Some(c);
                }
            }
        }
    }
    re.captures_at(hay, from)
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

/// The Unicode-aware equivalent of the POSIX character class `[:name:]`, for use inside a
/// bracket expression. `digit` and `xdigit` stay ASCII, like glibc's in a UTF-8 locale, and (also
/// like glibc) other decimal digits such as `٣` count as `alpha`. Unknown
/// names are passed through, so that `regex` reports them.
fn posix_class(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => r"\p{Alphabetic}[\p{Nd}--0-9]",
        "alnum" => r"\p{Alphabetic}\p{Nd}",
        "upper" => r"\p{Uppercase}",
        "lower" => r"\p{Lowercase}",
        "digit" => "0-9",
        "xdigit" => "0-9A-Fa-f",
        "space" => r"\p{White_Space}",
        "blank" => r"\t\p{Zs}",
        "punct" => r"\p{P}\p{S}",
        "cntrl" => r"\p{Cc}",
        "graph" => r"[^\p{C}\p{Z}]",
        "print" => r"[^\p{C}\p{Zl}\p{Zp}]",
        "word" => r"\w",
        _ => return None,
    })
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
                            let name: String = chars[j + 2..end].iter().collect();
                            match posix_class(&name) {
                                Some(class) => res.push_str(class),
                                None => res.extend(&chars[j..end + 2]),
                            }
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
        assert_eq!(translate("[[:alpha:]_]"), r"[\p{Alphabetic}[\p{Nd}--0-9]_]");
        assert_eq!(translate("[^[:digit:]]"), "[^0-9]");
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

    #[test]
    fn leftmost_longest() {
        let spans = |pat: &str, s: &str| -> Vec<(usize, usize)> {
            find_iter(&compile(pat).unwrap(), s.as_bytes()).collect()
        };
        assert_eq!(spans("a|ab", "abcd"), vec![(0, 2)]);
        assert_eq!(spans("x|xy", "xyz"), vec![(0, 2)]);
        assert_eq!(spans("o|oo", "foobar"), vec![(1, 3)]);
        assert_eq!(spans("y|", "x"), vec![(0, 0), (1, 1)]);
        assert_eq!(spans("ab", "abab"), vec![(0, 2), (2, 4)]);
        let re = compile("(x|xy)(z?)").unwrap();
        let caps = captures_at(&re, b"xyz", 0, 3).unwrap();
        assert_eq!(&caps[1], b"xy");
        assert_eq!(&caps[2], b"z");
    }

    #[test]
    fn unicode_posix_classes() {
        assert!(is_match("^[[:alpha:]]+$", "héllo中文"));
        assert!(!is_match("[[:alpha:]]", "123 !"));
        assert!(is_match("^[[:alnum:]]+$", "é9"));
        assert!(is_match("^[[:upper:]]$", "É"));
        assert!(is_match("^[[:lower:]]$", "é"));
        assert!(!is_match("[[:digit:]]", "٣"));
        assert!(is_match("[[:alpha:]]", "٣"));
        assert!(is_match("^[[:space:]]$", "\u{3000}"));
        assert!(is_match("^[[:blank:]]$", "\t"));
        assert!(is_match("^[[:punct:]]+$", "，。!$"));
        assert!(is_match("^[[:graph:]]+$", "中!"));
        assert!(!is_match("[[:graph:]]", " \t"));
        assert!(is_match("^[[:print:]]+$", "中 a"));
        assert!(!is_match("[[:print:]]", "\n"));
        assert!(is_match("^[^[:alpha:]]+$", "123"));
        assert!(!is_match("[^[:alpha:]]", "中文"));
    }
}
