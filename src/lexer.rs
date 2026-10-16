//! A custom lexer for AWK for use with LALRPOP.
//!
//! This lexer is fairly rudamentary. It ought not be too slow, but it also has not been optimized
//! very aggressively. Various edge cases still do not work.
use hashbrown::HashMap;
use std::borrow::Cow;
use lazy_static::lazy_static;
use regex::Regex;
use unicode_xid::UnicodeXID;

use crate::arena::Arena;

#[derive(PartialEq, Eq, Clone, Copy, Debug, Default)]
pub struct Loc {
    pub line: usize,
    pub col: usize,
    offset: usize,
}

impl Loc {
    /// The byte offset in the program text.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The location just past `text`, when `text` starts at this location.
    pub(crate) fn advance(self, text: &str) -> Loc {
        match text.rfind('\n') {
            Some(nl) => Loc {
                line: self.line + text.matches('\n').count(),
                col: text.len() - nl - 1,
                offset: self.offset + text.len(),
            },
            None => Loc {
                line: self.line,
                col: self.col + text.len(),
                offset: self.offset + text.len(),
            },
        }
    }
}

pub type Spanned<T> = (Loc, T, Loc);

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Tok<'a> {
    Begin,
    Prepare,
    BeginFile,
    EndFile,
    End,
    Break,
    Continue,
    Next,
    NextFile,
    For,
    If,
    Else,
    Print,
    Printf,
    // Separate token for a "print(" and "printf(".
    PrintLP,
    PrintfLP,
    Exit,
    ExitLP,
    While,
    Do,
    Switch,
    Case,
    Default,

    // { }
    LBrace,
    RBrace,
    // [ ]
    LBrack,
    RBrack,
    // ( )
    LParen,
    RParen,

    Getline,
    Assign,
    Add,
    AddAssign,
    Sub,
    SubAssign,
    Mul,
    MulAssign,
    Div,
    DivAssign,
    Pow,
    PowAssign,
    Mod,
    ModAssign,
    Match,
    NotMatch,

    EQ,
    NEQ,
    LT,
    GT,
    LTE,
    GTE,
    Incr,
    Decr,
    Not,

    AND,
    OR,
    QUESTION,
    Nullish,
    COLON,
    Pipe,

    Append, // >>

    Dollar,
    Semi,
    Newline,
    Comma,
    In,
    Delete,
    Return,

    Ident(&'a str),
    StrLit(&'a str),
    /// An f-string literal `f"Hello, {name}"`; the text between the quotes.
    FStrLit(&'a str),
    PatLit(&'a str),
    CallStart(&'a str),
    FunDec(&'a str),

    ILit(&'a str),
    HexLit(&'a str),
    FLit(&'a str),
}

macro_rules! keyword_map {
    ($name:ident<&'static [u8], $vty:ty>, $([$k:expr, $v:expr]),*) => {
        lazy_static::lazy_static! {
            pub(crate) static ref $name: hashbrown::HashMap<&'static [u8], $vty> = {
                let mut m = hashbrown::HashMap::new();
                $(
                    m.insert(&$k[..], $v);
                )*
                m
            };
        }
    }
}

keyword_map!(
    KEYWORDS<&'static [u8], Tok<'static>>,
    [b"PREPARE", Tok::Prepare],
    [b"BEGIN", Tok::Begin],
    [b"BEGINFILE", Tok::BeginFile],
    [b"ENDFILE", Tok::EndFile],
    [b"END", Tok::End],
    [b"break", Tok::Break],
    [b"continue", Tok::Continue],
    [b"next", Tok::Next],
    [b"nextfile", Tok::NextFile],
    [b"for", Tok::For],
    [b"if", Tok::If],
    [b"else", Tok::Else],
    [b"print", Tok::Print],
    [b"printf", Tok::Printf],
    [b"print(", Tok::PrintLP],
    [b"printf(", Tok::PrintfLP],
    [b"exit(", Tok::ExitLP],
    [b"exit", Tok::Exit],
    [b"while", Tok::While],
    [b"do", Tok::Do],
    [b"switch", Tok::Switch],
    [b"case", Tok::Case],
    [b"default", Tok::Default],
    [b"{", Tok::LBrace],
    [b"}", Tok::RBrace],
    [b"[", Tok::LBrack],
    [b"]", Tok::RBrack],
    [b"(", Tok::LParen],
    [b")", Tok::RParen],
    [b"getline", Tok::Getline],
    [b"|", Tok::Pipe],
    [b"=", Tok::Assign],
    [b"+", Tok::Add],
    [b"+=", Tok::AddAssign],
    [b"-", Tok::Sub],
    [b"-=", Tok::SubAssign],
    [b"*", Tok::Mul],
    [b"*=", Tok::MulAssign],
    [b"/", Tok::Div],
    [b"/=", Tok::DivAssign],
    [b"^", Tok::Pow],
    [b"^=", Tok::PowAssign],
    // gawk's (and onetrue awk's) spellings of `^` and `^=`.
    [b"**", Tok::Pow],
    [b"**=", Tok::PowAssign],
    [b"%", Tok::Mod],
    [b"%=", Tok::ModAssign],
    [b"~", Tok::Match],
    [b"!~", Tok::NotMatch],
    [b"==", Tok::EQ],
    [b"!=", Tok::NEQ],
    [b"<", Tok::LT],
    [b"<=", Tok::LTE],
    [b">", Tok::GT],
    [b"--", Tok::Decr],
    [b"++", Tok::Incr],
    [b">=", Tok::GTE],
    [b">>", Tok::Append],
    [b";", Tok::Semi],
    [b"\n", Tok::Newline],
    [b"\r\n", Tok::Newline],
    [b",", Tok::Comma],
    [b"in", Tok::In],
    [b"!", Tok::Not],
    [b"&&", Tok::AND],
    [b"||", Tok::OR],
    [b"?", Tok::QUESTION],
    [b"??", Tok::Nullish],
    [b":", Tok::COLON],
    [b"delete", Tok::Delete],
    [b"return", Tok::Return],
    [b"$", Tok::Dollar]
);

lazy_static! {
    // TODO: use a regex set for this instead. It'd be a bit faster and it would allow "must
    // follow"-style issues to be handled in a clearer way.
    static ref KEYWORDS_BY_LEN: Vec<HashMap<&'static [u8], Tok<'static>>> = {
        let max_len = KEYWORDS.keys().map(|s| s.len()).max().unwrap();
        let mut res: Vec<HashMap<_, _>> = vec![Default::default(); max_len];
        for (k, v) in KEYWORDS.iter() {
            res[k.len() - 1].insert(*k, v.clone());
        }
        res
    };
}

pub struct Tokenizer<'a> {
    text: &'a str,
    cur: usize,
    prev_tok: Option<Tok<'a>>,
    lines: Vec<usize>,
}

/// Scans the body of a regex literal (just after its opening '/'), and returns the index of the
/// closing '/', or of the first newline outside a bracket expression, whichever comes first. A '/'
/// inside a bracket expression does not end the regex (`/[/]/`, as in gawk). A ']' right after
/// '[' or '[^' is a literal ']', and '[:class:]' (also '[.x.]' and '[=x=]') may appear inside a
/// bracket expression.
fn regex_lit_end(bs: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut in_bracket = false;
    while i < bs.len() {
        match bs[i] {
            b'\\' => i += 1,
            b'/' | b'\n' if !in_bracket => return Some(i),
            b'[' if !in_bracket => {
                in_bracket = true;
                if bs.get(i + 1) == Some(&b'^') {
                    i += 1;
                }
                if bs.get(i + 1) == Some(&b']') {
                    i += 1;
                }
            }
            b'[' if matches!(bs.get(i + 1), Some(b':' | b'.' | b'=')) => {
                // Skip the class only when it is properly closed, e.g. '[:alpha:]'.
                let delim = bs[i + 1];
                if let Some(ix) = bs[i + 2..].iter().position(|&c| c == b']' || c == b'\n') {
                    let end = i + 2 + ix;
                    if bs[end] == b']' && ix > 0 && bs[end - 1] == delim {
                        i = end;
                    }
                }
            }
            b']' if in_bracket => in_bracket = false,
            // An unterminated bracket expression does not swallow the rest of the program.
            b'\n' => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// After `print` or `printf`: if `rest` is blanks followed by a parenthesized list with a
/// top-level comma, which is not followed by `in`, returns the length up to and including the
/// `(`. Such a list is the argument list (as in `print (a, b) > "file"`), rather than a grouping
/// (as in `print (a) (b)`) or a membership test (as in `print (a, b) in arr`).
fn paren_arg_list(rest: &str) -> Option<usize> {
    let bs = rest.as_bytes();
    let mut i = 0;
    while i < bs.len() && matches!(bs[i], b' ' | b'\t') {
        i += 1;
    }
    if i == 0 || bs.get(i) != Some(&b'(') {
        return None;
    }
    let open = i;
    let (mut parens, mut brackets) = (0usize, 0usize);
    let mut comma = false;
    // The last byte that was not a blank, to tell regex literals from division.
    let mut prev = b'(';
    while i < bs.len() {
        let c = bs[i];
        match c {
            b'"' => {
                i += 1;
                while i < bs.len() && bs[i] != b'"' {
                    i += if bs[i] == b'\\' { 2 } else { 1 };
                }
            }
            b'/' if matches!(
                prev,
                b'(' | b',' | b'~' | b'!' | b'&' | b'|' | b'=' | b'<' | b'>' | b'?' | b':'
                    | b'+' | b'-' | b'*' | b'%' | b'^' | b'['
            ) =>
            {
                i += 1;
                match regex_lit_end(&bs[i..]) {
                    Some(end) if bs[i + end] == b'/' => i += end,
                    _ => return None,
                }
            }
            b'(' => parens += 1,
            b')' => {
                parens -= 1;
                if parens == 0 {
                    break;
                }
            }
            b'[' => brackets += 1,
            b']' => brackets = brackets.saturating_sub(1),
            b',' if parens == 1 && brackets == 0 => comma = true,
            b'\n' => return None,
            _ => {}
        }
        if !matches!(c, b' ' | b'\t') {
            prev = c;
        }
        i += 1;
    }
    if !comma || i >= bs.len() {
        return None;
    }
    // `(a, b) in arr` is a membership test.
    let mut j = i + 1;
    while j < bs.len() && matches!(bs[j], b' ' | b'\t') {
        j += 1;
    }
    if bs[j..].starts_with(b"in") && !bs.get(j + 2).is_some_and(|c| is_id_body(*c as char)) {
        return None;
    }
    Some(open + 1)
}

pub fn is_ident(s: &str) -> bool {
    for (i, c) in s.chars().enumerate() {
        if i == 0 && !is_id_start(c) {
            return false;
        }
        if i > 0 && !is_id_body(c) {
            return false;
        }
    }
    true
}

fn is_id_start(c: char) -> bool {
    c == '_' || c.is_xid_start()
}

fn is_id_body(c: char) -> bool {
    c == '_' || c == '\'' || c.is_xid_continue()
}

fn push_char(buf: &mut Vec<u8>, c: char) {
    let start = buf.len();
    buf.resize_with(start + c.len_utf8(), Default::default);
    c.encode_utf8(&mut buf[start..]);
}

pub(crate) fn parse_string_literal<'a>(lit: &str, arena: &'a Arena, buf: &mut Vec<u8>) -> &'a [u8] {
    fn hex_digit(c: char) -> Option<u8> {
        match c {
            '0'..='9' => Some((c as u8) - b'0'),
            'a'..='f' => Some(10 + (c as u8) - b'a'),
            'A'..='F' => Some(10 + (c as u8) - b'A'),
            _ => None,
        }
    }
    fn octal_digit(c: char) -> Option<u8> {
        match c {
            '0'..='7' => Some((c as u8) - b'0'),
            _ => None,
        }
    }
    // assumes we just saw a '"'
    buf.clear();
    let mut is_escape = false;
    let mut iter = lit.chars();
    'top: while let Some(mut c) = iter.next() {
        'mid: loop {
            if is_escape {
                match c {
                    'a' => buf.push(0x07), // BEL
                    'b' => buf.push(0x08), // BS
                    'f' => buf.push(0x0C), // FF
                    'v' => buf.push(0x0B), // VT
                    '\\' => buf.push(b'\\'),
                    'n' => buf.push(b'\n'),
                    'r' => buf.push(b'\r'),
                    't' => buf.push(b'\t'),
                    '"' => buf.push(b'"'),
                    // 1 or 2 hex digits
                    'x' => {
                        let mut n = if let Some(x) = iter.next() {
                            if let Some(d) = hex_digit(x) {
                                d
                            } else {
                                // no digits, push \x and repeat.
                                buf.push(b'\\');
                                buf.push(b'x');
                                c = x;
                                is_escape = false;
                                continue;
                            }
                        } else {
                            buf.push(b'\\');
                            buf.push(b'x');
                            break 'top;
                        };
                        if let Some(x) = iter.next() {
                            if let Some(d) = hex_digit(x) {
                                // We cannot overflow
                                n *= 16;
                                n += d;
                            } else {
                                buf.push(n);
                                is_escape = false;
                                c = x;
                                continue;
                            }
                        }
                        buf.push(n)
                    }
                    // 1 to 3 octal digits
                    '0'..='7' => {
                        let mut n = octal_digit(c).unwrap();
                        for _ in 0..2 {
                            if let Some(x) = iter.next() {
                                if let Some(d) = octal_digit(x) {
                                    // saturate on overflow
                                    n = n.saturating_mul(8);
                                    n = n.saturating_add(d);
                                } else {
                                    buf.push(n);
                                    is_escape = false;
                                    c = x;
                                    continue 'mid;
                                }
                            }
                        }
                        buf.push(n);
                    }
                    // An unknown escape is the character itself, as in gawk and onetrue-awk:
                    // "\/" is "/" ("zawk lint" reports these, W102).
                    _ => push_char(buf, c),
                };
                is_escape = false;
            } else {
                match c {
                    '\\' => is_escape = true,
                    c => push_char(buf, c),
                }
            }
            break;
        }
    }
    arena.alloc_bytes(&buf[..])
}

/// The escapes of string literal text `lit` that are not escape sequences, as byte offsets of
/// their backslash and the escaped char: [`parse_string_literal`] drops their backslash.
pub(crate) fn unknown_escapes(lit: &str) -> Vec<(usize, char)> {
    let mut found = Vec::new();
    let mut chars = lit.char_indices();
    while let Some((i, c)) = chars.next() {
        if c != '\\' {
            continue;
        }
        match chars.next() {
            Some((_, 'a' | 'b' | 'f' | 'v' | '\\' | 'n' | 'r' | 't' | '"' | 'x' | '0'..='7')) | None => {}
            Some((_, c)) => found.push((i, c)),
        }
    }
    found
}

/// The pattern of a regex literal.
///
/// Regex literals and strings are both lowered to strings, but a single-character string used as a
/// separator (in FS and split()) is a literal character (and " " means runs of blanks), which
/// ignores IGNORECASE. A single-character regex literal whose meaning differs, such as `/./`, `/ /`
/// or `/x/` (which IGNORECASE applies to), must remain a regular expression, so it is wrapped in a
/// group, which does not change what it matches. Other characters (`/\//`) keep the fast literal
/// path.
pub(crate) fn parse_regex_literal<'a>(lit: &str, arena: &'a Arena, buf: &mut Vec<u8>) -> &'a [u8] {
    unescape_regex_literal(lit, buf);
    let mut chars = std::str::from_utf8(buf).map(|s| s.chars()).ok();
    let single_char = match chars.as_mut().map(|cs| (cs.next(), cs.next())) {
        Some((Some(c), None)) => {
            matches!(c, '.' | '^' | '$' | '|' | ' ') || c.to_lowercase().ne(c.to_uppercase())
        }
        _ => false,
    };
    if single_char {
        buf.splice(0..0, b"(?:".iter().copied());
        buf.push(b')');
    }
    arena.alloc_bytes(&buf[..])
}

/// Checks the syntax of a regex literal, so that an invalid one is a syntax error (as in gawk)
/// rather than a failure when compiling the program.
fn check_regex_literal(lit: &str) -> Result<(), String> {
    let mut buf = Vec::new();
    unescape_regex_literal(lit, &mut buf);
    // Patterns that are not valid UTF-8 are reported when the program is compiled.
    let Ok(pat) = std::str::from_utf8(&buf) else {
        return Ok(());
    };
    match crate::runtime::awk_regex::syntax_error(pat) {
        None => Ok(()),
        Some(reason) => Err(format!("invalid regex /{}/: {}", lit, reason)),
    }
}

fn unescape_regex_literal(lit: &str, buf: &mut Vec<u8>) {
    // Regexes have their own escaping rules, let them apply them. The only think we look to do is
    // replace "\/" with "/".
    // NB: Awk escaping rules are a subset of Rust's regex escape rule syntax, but if we applied
    // Awk's rewrites here, we might create a pattern that is invalid UTF-8, which will cause a
    // failure when we try and compile the regular expression.
    buf.clear();
    let mut is_escape = false;
    for c in lit.chars() {
        if is_escape {
            match c {
                '/' => buf.push(b'/'),
                c => {
                    buf.push(b'\\');
                    push_char(buf, c);
                }
            };
            is_escape = false;
        } else {
            match c {
                '\\' => {
                    is_escape = true;
                    continue;
                }
                c => {
                    push_char(buf, c);
                }
            }
        }
    }
}

impl<'a> Tokenizer<'a> {
    fn keyword<'c>(&self) -> Option<(Tok<'c>, usize)> {
        let start = self.cur;
        let remaining = self.text.len() - start;
        for (len, ks) in KEYWORDS_BY_LEN.iter().enumerate().rev() {
            let len = len + 1;
            if remaining < len {
                continue;
            }
            let text = &self.text.as_bytes()[start..start + len];
            if let Some(tok) = ks.get(text) {
                // A word keyword must end at an identifier boundary: `nextval` and `break_x` are
                // identifiers, while `break}`, `getline<` and `print"x"` start with a keyword.
                let ends_word = text.last().is_some_and(|c| is_id_body(*c as char));
                let continues_word = self.text[start + len..]
                    .chars()
                    .next()
                    .is_some_and(is_id_body);
                if !(ends_word && continues_word) {
                    return Some((tok.clone(), len));
                }
            }
        }
        None
    }

    fn num(&self) -> Option<(Tok<'a>, usize)> {
        lazy_static! {
            static ref HEX_PATTERN: Regex = Regex::new(r"^[+-]?0[xX][0-9A-Fa-f]+").unwrap();
            static ref INT_PATTERN: Regex = Regex::new(r"^[+-]?\d+").unwrap();
            // Adapted from https://www.regular-expressions.info/floatingpoint.html
            static ref FLOAT_PATTERN: Regex = Regex::new(r"^[-+]?(\d*\.\d+([eE][-+]?\d+)?|\d+[eE][-+]?\d+)").unwrap();
        };
        let text = &self.text[self.cur..];
        if let Some(i) = HEX_PATTERN.captures(text).and_then(|c| c.get(0)) {
            let is = i.as_str();
            return Some((Tok::HexLit(is), is.len()));
        } else if let Some(f) = FLOAT_PATTERN.captures(text).and_then(|c| c.get(0)) {
            let fs = f.as_str();
            Some((Tok::FLit(fs), fs.len()))
        } else if let Some(i) = INT_PATTERN.captures(text).and_then(|c| c.get(0)) {
            let is = i.as_str();
            Some((Tok::ILit(is), is.len()))
        } else {
            None
        }
    }

    fn fundec(&self) -> Option<(Tok<'a>, usize)> {
        lazy_static! {
            static ref FN_PATTERN: Regex =
                Regex::new(r"^((?:function|func)\s+([a-zA-Z_][a-zA-Z_0-9]*)[ \t]*)\(").unwrap();
        }
        let captures = FN_PATTERN.captures(&self.text[self.cur..])?;
        let full = captures.get(1)?.as_str();
        let name = captures.get(2)?.as_str();
        if KEYWORDS.get(name.as_bytes()).is_none() {
            Some((Tok::FunDec(name), full.len()))
        } else {
            None
        }
    }

    fn ident(&mut self, id_start: usize) -> (&'a str, usize) {
        debug_assert!(is_id_start(self.text[id_start..].chars().next().unwrap()));
        let ix = self.text[self.cur..]
            .char_indices()
            .take_while(|(_, c)| is_id_body(*c))
            .last()
            .map(|(ix, c)| self.cur + ix + c.len_utf8())
            .unwrap_or(self.cur);
        (&self.text[id_start..ix], ix)
    }

    fn literal(&mut self, delim: char, error_msg: &'static str) -> Result<(&'a str, usize), Error> {
        // assumes we just saw a delimiter.
        let mut bound = None;
        let mut is_escape = false;
        for (ix, c) in self.text[self.cur..].char_indices() {
            if is_escape {
                is_escape = false;
                continue;
            }
            if c == delim {
                bound = Some(ix);
                break;
            }
            if c == '\\' {
                is_escape = true;
            }
        }
        match bound {
            Some(end) => Ok((&self.text[self.cur..self.cur + end], self.cur + end + 1)),
            None => Err(Error {
                location: self.index_to_loc(self.cur),
                desc: error_msg.into(),
            }),
        }
    }

    fn regex_lit(&mut self) -> Result<(&'a str, usize /* new start */), Error> {
        // assumes we just saw a '/'.
        let text = &self.text[self.cur..];
        match regex_lit_end(text.as_bytes()) {
            Some(end) if text.as_bytes()[end] == b'/' => Ok((&text[..end], self.cur + end + 1)),
            _ => Err(Error {
                location: self.index_to_loc(self.cur),
                desc: "incomplete regex literal".into(),
            }),
        }
    }

    fn string_lit(&mut self) -> Result<(&'a str, usize /* new start */), Error> {
        self.literal('"', "incomplete string literal")
    }

    fn consume_comment(&mut self) {
        let mut iter = self.text[self.cur..].char_indices();
        if let Some((_, '#')) = iter.next() {
            if let Some((ix, _)) = iter.find(|x| x.1 == '\n') {
                self.cur += ix;
            } else {
                self.cur = self.text.len();
            }
        }
    }

    fn consume_ws(&mut self) {
        let text = &self.text[self.cur..];
        // Offset just past the last consumed whitespace.
        let mut end = 0;
        let mut iter = text.char_indices();
        while let Some((ix, c)) = iter.next() {
            if c == '\\' {
                // A backslash before a newline (or CRLF) is a line continuation.
                let rest = &text[ix + 1..];
                let cont = if rest.starts_with('\n') {
                    1
                } else if rest.starts_with("\r\n") {
                    2
                } else {
                    break;
                };
                end = ix + 1 + cont;
                for _ in 0..cont {
                    iter.next();
                }
                continue;
            }
            // A byte order mark (from editors on Windows) is skipped like whitespace.
            if c == '\n' || !(c.is_whitespace() || c == '\u{feff}') {
                break;
            }
            end = ix + c.len_utf8();
        }
        self.cur += end;
    }

    fn advance(&mut self) {
        let mut prev = self.cur;
        loop {
            self.consume_ws();
            self.consume_comment();
            if self.cur == prev {
                break;
            }
            prev = self.cur;
        }
    }

    fn potential_re(&self) -> bool {
        !matches!(
            &self.prev_tok,
            Some(Tok::Ident(_))
                | Some(Tok::StrLit(_))
                | Some(Tok::FStrLit(_))
                | Some(Tok::PatLit(_))
                | Some(Tok::ILit(_))
                | Some(Tok::HexLit(_))
                | Some(Tok::FLit(_))
                | Some(Tok::RParen)
                | Some(Tok::RBrack)
                // `x++ / 2`: a regex never directly follows `++`/`--`
                | Some(Tok::Incr)
                | Some(Tok::Decr)
        )
    }
}

#[derive(Debug)]
pub struct Error {
    pub location: Loc,
    pub desc: Cow<'static, str>,
}

impl From<&'static str> for Error {
    fn from(s: &'static str) -> Error {
        Error {
            location: Default::default(),
            desc: s.into(),
        }
    }
}

impl<'a> Tokenizer<'a> {
    pub fn new(text: &'a str) -> Tokenizer<'a> {
        Tokenizer {
            // A hack to get around some programs failing to parse due to a trailing newline
            text: text.trim_end_matches('\n'),
            cur: 0,
            prev_tok: None,
            lines: text
                .as_bytes()
                .iter()
                .enumerate()
                .flat_map(|(i, b)| if *b == b'\n' { Some(i) } else { None }.into_iter())
                .collect(),
        }
    }
    fn index_to_loc(&self, ix: usize) -> Loc {
        let offset = ix;
        match self.lines.binary_search(&ix) {
            Ok(0) | Err(0) => Loc {
                line: 0,
                col: ix,
                offset,
            },
            // `ix` is the newline ending the line.
            Ok(line) => Loc {
                line,
                col: ix - self.lines[line - 1] - 1,
                offset,
            },
            Err(line) => Loc {
                line,
                col: ix - self.lines[line - 1] - 1,
                offset,
            },
        }
    }
    fn spanned<T>(&self, l: usize, r: usize, t: T) -> Spanned<T> {
        (self.index_to_loc(l), t, self.index_to_loc(r))
    }
}

impl<'a> Iterator for Tokenizer<'a> {
    type Item = Result<Spanned<Tok<'a>>, Error>;
    fn next(&mut self) -> Option<Result<Spanned<Tok<'a>>, Error>> {
        macro_rules! try_tok {
            ($e:expr) => {
                match $e {
                    Ok(e) => e,
                    Err(e) => return Some(Err(e)),
                }
            };
        }
        self.advance();
        let span = if let Some((ix, c)) = self.text[self.cur..].char_indices().next() {
            let ix = self.cur + ix;
            match c {
                '}' => {
                    // In awk statements are terminated by semicolons, newlines or right braces.
                    // The current lalrpop grammar does not support parsing statements that are
                    // terminated by a right brace from an upper level, so a ';' gets injected
                    // before a '}' if none of the following conditions occurs:
                    //   - Previous token is not a newline or ';'.
                    //   - Previous token is not a '{', as "{}" should not be converted to
                    //     invalid "{;}".
                    match self.prev_tok {
                        Some(Tok::Semi)|Some(Tok::Newline)|Some(Tok::LBrace) => {
                            self.cur += 1;
                            self.spanned(ix, self.cur, Tok::RBrace)
                        },
                        _ => self.spanned(ix, self.cur, Tok::Semi),
                    }
                }
                '"' => {
                    self.cur += 1;
                    let (s, new_start) = try_tok!(self.string_lit());
                    self.cur = new_start;
                    self.spanned(ix, new_start, Tok::StrLit(s))
                }
                '/' if self.potential_re() => {
                    self.cur += 1;
                    let (re, new_start) = try_tok!(self.regex_lit());
                    if let Err(desc) = check_regex_literal(re) {
                        return Some(Err(Error {
                            location: self.index_to_loc(ix),
                            desc: desc.into(),
                        }));
                    }
                    self.cur = new_start;
                    self.spanned(ix, new_start, Tok::PatLit(re))
                }
                'f' if self.text.as_bytes().get(ix + 1) == Some(&b'"') => {
                    self.cur += 2;
                    let (s, new_start) = try_tok!(self.string_lit());
                    self.cur = new_start;
                    self.spanned(ix, new_start, Tok::FStrLit(s))
                }
                c => {
                    if let Some((tok, len)) = self.fundec() {
                        self.cur += len;
                        self.spanned(ix, self.cur, tok)
                    } else if let Some((tok, len)) = self.keyword() {
                        // `print (a, b) > "file"`: a parenthesized argument list after a space,
                        // like `print(a, b)`.
                        let (tok, len) = match tok {
                            Tok::Print | Tok::Printf => {
                                match paren_arg_list(&self.text[self.cur + len..]) {
                                    Some(n) if matches!(tok, Tok::Print) => (Tok::PrintLP, len + n),
                                    Some(n) => (Tok::PrintfLP, len + n),
                                    None => (tok, len),
                                }
                            }
                            tok => (tok, len),
                        };
                        self.cur += len;
                        self.spanned(ix, self.cur, tok)
                    } else if let Some((tok, len)) = self.num() {
                        self.cur += len;
                        self.spanned(ix, self.cur, tok)
                    } else if is_id_start(c) {
                        self.cur += c.len_utf8();
                        let (s, new_start) = self.ident(ix);
                        let bs = self.text.as_bytes();
                        if new_start < bs.len() && self.text.as_bytes()[new_start] == b'(' {
                            self.cur = new_start + 1;
                            self.spanned(ix, self.cur, Tok::CallStart(s))
                        } else {
                            self.cur = new_start;
                            self.spanned(ix, self.cur, Tok::Ident(s))
                        }
                    } else {
                        // Report unknown characters instead of ending the token stream, which
                        // would silently drop the rest of the program.
                        return Some(Err(Error {
                            location: self.index_to_loc(ix),
                            desc: if c == '@' {
                                "unsupported '@' syntax (@include, @load, @namespace and indirect function calls are not supported)"
                            } else {
                                "unexpected character"
                            }
                            .into(),
                        }));
                    }
                }
            }
        } else if let Some(Tok::Newline) = self.prev_tok {
            return None;
        } else {
            self.spanned(self.cur, self.cur, Tok::Newline)
        };
        self.prev_tok = Some(span.1.clone());
        Some(Ok(span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lex_str(s: &str) -> Vec<Spanned<Tok<'_>>> {
        Tokenizer::new(s).map(|x| x.ok().unwrap()).collect()
    }

    #[test]
    fn locations() {
        const TEXT: &str = r#"This is the first line
and the second
and the third"#;
        let tok = Tokenizer::new(TEXT);
        assert_eq!(
            tok.index_to_loc(4),
            Loc {
                line: 0,
                col: 4,
                offset: 4,
            }
        );
        assert_eq!(
            tok.index_to_loc(22),
            Loc {
                line: 0,
                col: 22,
                offset: 22,
            }
        );
        assert_eq!(
            tok.index_to_loc(23),
            Loc {
                line: 1,
                col: 0,
                offset: 23,
            }
        );
        let tok2 = Tokenizer::new("\nhello");
        assert_eq!(
            tok2.index_to_loc(0),
            Loc {
                line: 0,
                col: 0,
                offset: 0
            },
        );
        assert_eq!(
            tok2.index_to_loc(1),
            Loc {
                line: 1,
                col: 0,
                offset: 1
            },
        );
        assert_eq!(
            tok2.index_to_loc(2),
            Loc {
                line: 1,
                col: 1,
                offset: 2
            },
        );
    }

    #[test]
    fn basic() {
        let toks = lex_str(
            r#" if (x == yzk){
            print x<y, y<=z, z;
        }"#,
        );
        use Tok::*;
        assert_eq!(
            toks.into_iter().map(|x| x.1).collect::<Vec<_>>(),
            vec![
                If,
                LParen,
                Ident("x"),
                EQ,
                Ident("yzk"),
                RParen,
                LBrace,
                Newline,
                Print,
                Ident("x"),
                LT,
                Ident("y"),
                Comma,
                Ident("y"),
                LTE,
                Ident("z"),
                Comma,
                Ident("z"),
                Semi,
                Newline,
                RBrace,
                Newline,
            ]
        );
    }

    #[test]
    fn keyword_boundary() {
        use Tok::*;
        // Without the newline that ends the input.
        let toks = |s| {
            let mut v = lex_str(s).into_iter().map(|x| x.1).collect::<Vec<_>>();
            if v.last() == Some(&Newline) {
                v.pop();
            }
            v
        };
        assert_eq!(toks("break}"), vec![Break, Semi, RBrace]);
        assert_eq!(toks("continue}"), vec![Continue, Semi, RBrace]);
        assert_eq!(toks("getline<f"), vec![Getline, LT, Ident("f")]);
        assert_eq!(toks("return}"), vec![Return, Semi, RBrace]);
        assert_eq!(toks("print\"x\""), vec![Print, StrLit("x")]);
        assert_eq!(toks("k in a"), vec![Ident("k"), In, Ident("a")]);
        assert_eq!(
            toks("switch(x){case /a/:default:}"),
            vec![Switch, LParen, Ident("x"), RParen, LBrace, Case, PatLit("a"), COLON, Default, COLON, Semi, RBrace]
        );
        assert_eq!(
            toks("switches cases defaults"),
            vec![Ident("switches"), Ident("cases"), Ident("defaults")]
        );
        assert_eq!(
            toks("nextval exit_code elsewhere done index"),
            vec![
                Ident("nextval"),
                Ident("exit_code"),
                Ident("elsewhere"),
                Ident("done"),
                Ident("index"),
            ]
        );
    }

    #[test]
    fn literals() {
        let toks =
            lex_str(r#" x="\"hi\tthere\n"; b   =/hows it \/going/; x="重庆辣子鸡"; c= 1 / 3.5 "#);
        use Tok::*;
        let s1 = "\\\"hi\\tthere\\n";
        let s2 = "hows it \\/going";
        assert_eq!(
            toks.into_iter().map(|x| x.1).collect::<Vec<_>>(),
            vec![
                Ident("x"),
                Assign,
                StrLit(s1),
                Semi,
                Ident("b"),
                Assign,
                PatLit(s2),
                Semi,
                Ident("x"),
                Assign,
                StrLit("重庆辣子鸡"),
                Semi,
                Ident("c"),
                Assign,
                ILit("1"),
                Div,
                FLit("3.5"),
                Newline,
            ],
        );
        let mut buf = Vec::new();
        let a = Arena::default();
        assert_eq!(parse_string_literal(s1, &a, &mut buf), b"\"hi\tthere\n");
        assert_eq!(parse_regex_literal(s2, &a, &mut buf), b"hows it /going");
        assert_eq!(
            parse_string_literal(r#"are you there \77\x3f"#, &a, &mut buf),
            b"are you there ??"
        );
        assert_eq!(
            parse_string_literal(r#"are you there \77\x"#, &a, &mut buf),
            b"are you there ?\\x"
        );
        assert_eq!(
            parse_string_literal(r#"are you there \77\xh"#, &a, &mut buf),
            b"are you there ?\\xh"
        );
        assert_eq!(parse_string_literal(r#"a\qb x\/y \. \\."#, &a, &mut buf), b"aqb x/y . \\.");
        assert_eq!(unknown_escapes(r#"a\qb \\. \t\/"#), [(1, 'q'), (11, '/')]);
    }

    #[test]
    fn unknown_character_is_an_error() {
        for (text, desc) in [
            ("BEGIN { x = 1 } `rest", "unexpected character"),
            ("@namespace \"foo\"\nBEGIN { x = 1 }", "unsupported '@' syntax"),
        ] {
            let err = Tokenizer::new(text)
                .find_map(Result::err)
                .unwrap_or_else(|| panic!("no error for {:?}", text));
            assert!(err.desc.starts_with(desc), "{:?}: {}", text, err.desc);
        }
    }

    #[test]
    fn division_after_numeric_literal() {
        use Tok::*;
        for (text, lit) in [
            ("1 / 2 / 1", ILit("1")),
            ("0x10 / 2 / 1", HexLit("0x10")),
            ("1.5 / 2 / 1", FLit("1.5")),
        ] {
            let toks: Vec<_> = lex_str(text).into_iter().map(|x| x.1).collect();
            assert_eq!(
                &toks[..5],
                &[lit, Div, ILit("2"), Div, ILit("1")],
                "{:?}",
                text
            );
        }
    }

    #[test]
    fn trailing_whitespace_and_bom() {
        use Tok::*;
        for text in [
            "x = 1 ",
            "x = 1\r\n",
            "x = 1\t\r",
            "\u{feff}x = 1",
            "x = \\\r\n1",
        ] {
            let toks: Vec<_> = lex_str(text).into_iter().map(|x| x.1).collect();
            assert_eq!(&toks[..3], &[Ident("x"), Assign, ILit("1")], "{:?}", text);
        }
    }

    #[test]
    fn slash_in_regex_bracket() {
        use Tok::*;
        for (text, re) in [
            ("/[/]/", "[/]"),
            ("/[a/]b/", "[a/]b"),
            ("/[]/]x/", "[]/]x"),
            ("/[^]/]+/", "[^]/]+"),
            ("/[[:alpha:]/]+/", "[[:alpha:]/]+"),
            ("/[\\/]/", "[\\/]"),
            ("/a\\/b/", "a\\/b"),
            ("/[:/]/", "[:/]"),
        ] {
            let toks: Vec<_> = lex_str(text).into_iter().map(|x| x.1).collect();
            assert_eq!(&toks[..1], &[PatLit(re)], "{:?}", text);
        }
        let mut buf = Vec::new();
        unescape_regex_literal("[/]a\\/b", &mut buf);
        assert_eq!(&buf[..], b"[/]a/b");
        assert!(Tokenizer::new("/[a/\n").any(|t| t.is_err()));
    }
}
