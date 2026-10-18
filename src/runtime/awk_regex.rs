//! Compiling awk (POSIX ERE, with gawk's extensions) regular expressions with the `regex` crate.
//!
//! The syntaxes mostly agree; `translate` rewrites the parts that do not:
//!
//! * `.` matches any character, including a newline (set on the builder, see `compile`).
//! * A `{` that does not start a valid interval (`{n}`, `{n,}`, `{n,m}`) is a literal, as is one
//!   at the start of an expression.
//! * gawk's operators: `\y` (word boundary), `\<` and `\>` (start/end of a word), `` \` `` and
//!   `\'` (start/end of the string), and `\B`.
//! * Escapes: `\b` is a backspace (gawk uses `\y` for word boundaries), `\a`, `\v`, `\f`; octal
//!   (`\351`) and hex (`\xE9`) escapes are bytes, not code points (see `push_byte`); other
//!   letters that are not operators are literal characters.
//! * POSIX character classes (`[:alpha:]` etc.) match non-ASCII characters too, as in gawk under a
//!   UTF-8 locale (the `regex` crate's own versions are ASCII-only); see `posix_class`.
//! * In bracket expressions, `[`, `&`, `~` and `-` sequences that the `regex` crate treats as
//!   nested classes or set operations are literal characters.
//!
//! `regex` reports the leftmost-first match of an alternation (e.g. `x` for `x|xy` against
//! "xyz"), where POSIX awk reports the leftmost-longest one (`xy`). The `find_at`, `find_iter`
//! and `captures_at` helpers below restore the POSIX semantics for patterns with an alternation or
//! a repetition of a group (`(ab)?(abcd)?`); other patterns use `regex` directly.
use regex::bytes::{Captures, Regex, RegexBuilder};
use regex_automata::hybrid::dfa::{Cache, DFA};
use regex_automata::{Anchored, Input, MatchKind};
use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

thread_local! {
    // gawk's IGNORECASE, set when the variable is assigned (see `Variables::store_int`). Worker
    // threads copy it from the main thread when they start (see `Core::shuttle`).
    static IGNORE_CASE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

// Whether IGNORECASE was ever set on any thread. String comparisons and index() check this first,
// so programs that do not use IGNORECASE do not pay for the thread-local lookup.
static IGNORE_CASE_USED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set whether regexes compiled, string comparisons and index() on this thread ignore case
/// (gawk's IGNORECASE).
pub(crate) fn set_ignore_case(on: bool) {
    if on {
        IGNORE_CASE_USED.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    IGNORE_CASE.with(|c| c.set(on));
}

/// Whether IGNORECASE is set on this thread.
#[inline]
pub(crate) fn ignore_case() -> bool {
    IGNORE_CASE_USED.load(std::sync::atomic::Ordering::Relaxed) && IGNORE_CASE.with(|c| c.get())
}

/// Prefix `pat` (in the syntax of `regex`) with `(?i)` when IGNORECASE is set. The flag is part of
/// the pattern text so that the regexes derived from `re.as_str()` below ignore case as well.
pub(crate) fn with_case(pat: String) -> String {
    if ignore_case() {
        format!("(?i){}", pat)
    } else {
        pat
    }
}

/// Compile an awk regular expression.
pub(crate) fn compile(pat: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(&with_case(translate(pat)))
        .dot_matches_new_line(true)
        .build()
}

/// Where a pattern without regex operators (see `as_literal`) has to match its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Literal {
    /// `lit`: anywhere.
    Contains,
    /// `^lit`
    Prefix,
    /// `lit$`
    Suffix,
    /// `^lit$`
    Exact,
}

/// When `pat` is literal text, optionally anchored with `^` and `$` (and IGNORECASE is off),
/// where it has to match and the text. Such patterns, which programs often build from their
/// input (`$2 ~ ("^" $1 "$")`), are matched without compiling (or caching) a regex.
pub(crate) fn as_literal(pat: &[u8]) -> Option<(Literal, &[u8])> {
    if ignore_case() || std::str::from_utf8(pat).is_err() {
        return None;
    }
    let (start, rest) = match pat.strip_prefix(b"^") {
        Some(rest) => (true, rest),
        None => (false, pat),
    };
    let (end, lit) = match rest.strip_suffix(b"$") {
        Some(lit) => (true, lit),
        None => (false, rest),
    };
    let is_meta = |b: &u8| {
        matches!(
            b,
            b'\\' | b'.' | b'[' | b']' | b'(' | b')' | b'*' | b'+' | b'?' | b'{' | b'}' | b'|' | b'^' | b'$'
        )
    };
    if lit.iter().any(is_meta) {
        return None;
    }
    let kind = match (start, end) {
        (false, false) => Literal::Contains,
        (true, false) => Literal::Prefix,
        (false, true) => Literal::Suffix,
        (true, true) => Literal::Exact,
    };
    Some((kind, lit))
}

/// The leftmost match of the literal pattern `(kind, lit)` (see `as_literal`) in `hay`, as
/// (start, end).
pub(crate) fn find_literal(kind: Literal, lit: &[u8], hay: &[u8]) -> Option<(usize, usize)> {
    let n = lit.len();
    match kind {
        Literal::Contains => memchr::memmem::find(hay, lit).map(|i| (i, i + n)),
        Literal::Prefix => hay.starts_with(lit).then_some((0, n)),
        Literal::Suffix => hay.ends_with(lit).then(|| (hay.len() - n, hay.len())),
        Literal::Exact => (hay == lit).then_some((0, n)),
    }
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

/// Memory budget, in bytes, for the patterns kept in `LONGEST`. Like `RegexCache` (see
/// `REGEX_CACHE_BUDGET`), patterns can come from input data, so once the estimated size of the
/// cache reaches this budget the least recently used half of it is evicted.
const LONGEST_BUDGET: usize = 64 << 20;

/// Estimated memory of the longest-match machinery for a pattern of the given length in bytes.
const LONGEST_COST: super::CostModel = super::CostModel {
    base: 8 << 10,
    per_byte: 512,
};

thread_local! {
    /// The longest-match machinery per pattern.
    static LONGEST: RefCell<super::StrLru<Option<Rc<Longest>>>> =
        RefCell::new(super::StrLru::new(LONGEST_BUDGET, LONGEST_COST));
}

/// Whether the leftmost-first match of `pat` (in the syntax of `regex`) can be shorter than the
/// leftmost-longest one: the pattern has an alternation, or a repetition of something other than a
/// single character (`(ab)?(abcd)?`) or that is not greedy. Patterns that fail to parse are
/// assumed to need it.
fn needs_longest(pat: &str) -> bool {
    use regex_syntax::hir::{Hir, HirKind};
    fn one_char(h: &Hir) -> bool {
        match h.kind() {
            HirKind::Class(_) => true,
            HirKind::Literal(lit) => std::str::from_utf8(&lit.0).map_or(lit.0.len() == 1, |s| s.chars().count() == 1),
            HirKind::Capture(c) => one_char(&c.sub),
            _ => false,
        }
    }
    fn walk(h: &Hir) -> bool {
        match h.kind() {
            HirKind::Alternation(_) => true,
            HirKind::Repetition(r) => !r.greedy || !one_char(&r.sub),
            HirKind::Capture(c) => walk(&c.sub),
            HirKind::Concat(hs) => hs.iter().any(walk),
            _ => false,
        }
    }
    regex_syntax::ParserBuilder::new()
        .utf8(false)
        .dot_matches_new_line(true)
        .build()
        .parse(pat)
        .map_or(true, |h| walk(&h))
}

/// The longest-match machinery for `re`, or `None` when leftmost-first is already leftmost-longest
/// (see `needs_longest`) or the DFA cannot be built.
fn longest(re: &Regex) -> Option<Rc<Longest>> {
    let pat = re.as_str();
    LONGEST.with(|cell| {
        if let Some(l) = cell.borrow_mut().get(pat) {
            return l.clone();
        }
        if !needs_longest(pat) {
            cell.borrow_mut().insert(pat.to_owned(), None);
            return None;
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
        cell.borrow_mut().insert(pat.to_owned(), l.clone());
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

/// The value of the octal escape (up to three digits) at `chars[*i]`.
fn parse_octal(chars: &[char], i: &mut usize) -> u8 {
    let mut value = 0u32;
    let mut n = 0;
    while n < 3 && *i < chars.len() && ('0'..='7').contains(&chars[*i]) {
        value = value * 8 + chars[*i].to_digit(8).unwrap();
        *i += 1;
        n += 1;
    }
    (value & 0xff) as u8
}

/// The value of the hex escape (one or two digits, as in gawk) at `chars[*i]`, just after `\x`.
fn parse_hex(chars: &[char], i: &mut usize) -> Option<u8> {
    let mut value = 0u32;
    let mut n = 0;
    while n < 2 && *i < chars.len() && chars[*i].is_ascii_hexdigit() {
        value = value * 16 + chars[*i].to_digit(16).unwrap();
        *i += 1;
        n += 1;
    }
    (n > 0).then_some(value as u8)
}

/// Push the byte escape `b` (from `\351` or `\xE9`). As in gawk, it matches a byte, not the
/// character U+00E9: ASCII values are the same either way, the others are matched with Unicode
/// mode off. Inside a bracket expression the escape is written as `\xE9`, and
/// `translate_bracket` moves it out of the Unicode part of the class.
fn push_byte(out: &mut String, b: u8, in_bracket: bool) {
    if b < 0x80 {
        out.push_str(&format!("\\x{{{:x}}}", b));
    } else if in_bracket {
        out.push_str(&format!("\\x{:02X}", b));
    } else {
        out.push_str(&format!("(?-u:\\x{:02X})", b));
    }
}

/// The byte escape (`\351` or `\xE9`) at `chars[*i]` (a backslash), if any, consuming it.
fn byte_escape(chars: &[char], i: &mut usize) -> Option<u8> {
    if chars.get(*i) != Some(&'\\') {
        return None;
    }
    let mut j = *i + 1;
    let b = match chars.get(j) {
        Some('0'..='7') => parse_octal(chars, &mut j),
        Some('x') if chars.get(j + 1) != Some(&'{') => {
            j += 1;
            parse_hex(chars, &mut j)?
        }
        _ => return None,
    };
    *i = j;
    Some(b)
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
        // Unicode classes and code points, e.g. \p{Greek} or \x{e9}, as supported by `regex`.
        'p' | 'P' => {
            out.push('\\');
            out.push(c);
        }
        'x' if chars.get(*i) == Some(&'{') => {
            out.push_str("\\x");
            while let Some(&c) = chars.get(*i) {
                out.push(c);
                *i += 1;
                if c == '}' {
                    break;
                }
            }
        }
        // gawk's hex escapes (one or two digits) are bytes, like octal escapes.
        'x' => match parse_hex(chars, i) {
            Some(b) => push_byte(out, b, in_bracket),
            None => out.push('x'),
        },
        '0'..='7' => {
            *i -= 1;
            let b = parse_octal(chars, i);
            push_byte(out, b, in_bracket);
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
    // Byte escapes of non-ASCII values (and ranges of them), matched with Unicode mode off.
    let mut bytes = String::new();
    let negated = chars.get(j) == Some(&'^');
    if negated {
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
                if let Some(k) = high_byte_item(&mut bytes, chars, j) {
                    j = k;
                    continue;
                }
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
    if bytes.is_empty() {
        out.push_str(&res);
    } else {
        let body = &res[if negated { 2 } else { 1 }..res.len() - 1];
        let neg = if negated { "^" } else { "" };
        let byte_safe = body.is_ascii()
            && !["\\p", "\\P", "\\w", "\\W", "\\d", "\\D", "\\s", "\\S"].iter().any(|e| body.contains(e));
        if byte_safe {
            // ASCII is the same with Unicode mode on or off, so the whole class can be bytes.
            out.push_str(&format!("(?-u:[{}{}{}])", neg, body, bytes));
        } else if !negated {
            out.push_str(&format!("(?:{}|(?-u:[{}]))", res, bytes));
        } else {
            // A negated class mixing Unicode classes and bytes cannot be split into an
            // alternation; fall back to treating the bytes as code points.
            out.push_str(&format!("[^{}{}]", body, bytes));
        }
    }
    *i = j;
    true
}

/// If the bracket expression item at `chars[j]` (a backslash) is a byte escape of a non-ASCII
/// value, or a range with such an end (`\200-\377`), push it to `bytes` in the syntax of `regex`
/// and return the index after it.
fn high_byte_item(bytes: &mut String, chars: &[char], j: usize) -> Option<usize> {
    let mut k = j;
    let lo = byte_escape(chars, &mut k)?;
    let mut hi = None;
    if chars.get(k) == Some(&'-') && chars.get(k + 1).is_some_and(|&c| c != ']') {
        let mut m = k + 1;
        hi = match byte_escape(chars, &mut m) {
            Some(b) => Some(b),
            None if chars[m] != '\\' && chars[m].is_ascii() => {
                m += 1;
                Some(chars[m - 1] as u8)
            }
            None => None,
        };
        if hi.is_some() {
            k = m;
        }
    }
    if lo < 0x80 && hi.map_or(true, |h| h < 0x80) {
        return None;
    }
    bytes.push_str(&format!("\\x{:02X}", lo));
    if let Some(h) = hi {
        bytes.push_str(&format!("-\\x{:02X}", h));
    }
    Some(k)
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
        assert_eq!(translate(r"\351"), r"(?-u:\xE9)");
        assert_eq!(translate(r"\xc3\xa9"), r"(?-u:\xC3)(?-u:\xA9)");
        assert_eq!(translate(r"\x41\xg"), r"\x{41}xg");
        assert_eq!(translate(r"\x{e9}"), r"\x{e9}");
        assert_eq!(translate(r"[a\351]"), r"(?-u:[a\xE9])");
        assert_eq!(translate(r"[^\200-\377]"), r"(?-u:[^\x80-\xFF])");
        assert_eq!(translate(r"[é\351]"), r"(?:[é]|(?-u:[\xE9]))");
    }

    #[test]
    fn byte_escapes() {
        assert!(is_match(r"\303\251", "é"));
        assert!(is_match(r"\xc3\xa9", "é"));
        assert!(!is_match(r"^\351", "é"));
        assert!(compile(r"^\351$").unwrap().is_match(b"\xe9"));
        assert!(compile(r"^[\200-\377]+$").unwrap().is_match("中文".as_bytes()));
        assert!(!is_match(r"[\200-\377]", "abc"));
        assert!(is_match(r"^[^\200-\377]+$", "abc"));
        assert!(is_match(r"^[[:alpha:]\351]$", "é"));
        assert!(compile(r"^[[:alpha:]\351]$").unwrap().is_match(b"\xe9"));
        assert!(is_match(r"^\x{e9}$", "é"));
        assert!(is_match(r"\101\x42", "AB"));
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
        // Repeated groups choose between alternatives too.
        assert_eq!(spans("(ab)?(abcd)?", "abcd")[0], (0, 4));
        assert_eq!(spans("x(y)?(yz)?", "xyz"), vec![(0, 3)]);
        assert_eq!(spans("(a|ab)(c|bcd)", "abcd"), vec![(0, 4)]);
        let re = compile("(ab)?(abcd)?").unwrap();
        let caps = captures_at(&re, b"abcd", 0, 4).unwrap();
        assert!(caps.get(1).is_none());
        assert_eq!(&caps[2], b"abcd");
    }

    #[test]
    fn needs_longest_from_hir() {
        assert!(needs_longest("a|ab"));
        assert!(needs_longest("(ab)?(abcd)?"));
        assert!(needs_longest("(?:ab)*a"));
        assert!(!needs_longest("[|]"));
        assert!(!needs_longest(r"\|"));
        assert!(!needs_longest("a|b"));
        assert!(!needs_longest("x?y*[a-z]+(é)?(.){2,3}"));
    }

    /// Patterns that `needs_longest` lets through use leftmost-first directly: check against the
    /// longest match from the all-matches DFA on every short haystack.
    #[test]
    fn leftmost_first_is_longest_when_skipped() {
        let atoms = ["a", "b", "a?", "b?", "a*", "b+", "[ab]?", "(a)?", ".?"];
        let hays: Vec<String> = (0..4)
            .flat_map(|n| (0..1 << n).map(move |m| (0..n).map(|i| if m >> i & 1 == 1 { 'b' } else { 'a' }).collect()))
            .collect();
        for x in atoms {
            for y in atoms {
                for z in atoms {
                    let pat = format!("{}{}{}", x, y, z);
                    assert!(!needs_longest(&pat), "{}", pat);
                    let re = compile(&pat).unwrap();
                    let dfa = DFA::builder()
                        .configure(DFA::config().match_kind(MatchKind::All))
                        .build(&pat)
                        .unwrap();
                    let l = Longest { cache: RefCell::new(dfa.create_cache()), dfa, exact: OnceCell::new() };
                    for hay in &hays {
                        if let Some(m) = re.find(hay.as_bytes()) {
                            assert_eq!(longest_end(&l, hay.as_bytes(), m.start()), Some(m.end()), "/{}/ {:?}", pat, hay);
                        }
                    }
                }
            }
        }
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

    #[test]
    fn longest_cache_is_bounded() {
        const BUDGET: usize = 2 << 20;
        LONGEST.with(|cell| cell.borrow_mut().budget = BUDGET);
        let hot = compile("hot|hotter").unwrap();
        for i in 0..(BUDGET / LONGEST_COST.cost(8) * 3) {
            assert_eq!(find_at(&hot, b"hotter", 0), Some((0, 6)));
            let re = compile(&format!("zz|x{}", i)).unwrap();
            assert_eq!(find_at(&re, format!("x{}", i).as_bytes(), 0).map(|(f, _)| f), Some(0));
            LONGEST.with(|cell| {
                let cache = cell.borrow();
                assert!(cache.used <= BUDGET);
                // A frequently used pattern survives evictions.
                assert!(cache.cached.contains_key(hot.as_str()));
            });
        }
        // Evicted patterns are rebuilt and still match leftmost-longest.
        let re = compile("x|x0").unwrap();
        assert_eq!(find_at(&re, b"x0", 0), Some((0, 2)));
    }
}
