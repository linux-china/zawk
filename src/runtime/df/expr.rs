//! The expression language of `df_filter`, `df_mutate` and the aggregates of `df_group`: a subset
//! of awk expressions over the columns of a DataFrame.
//!
//! * columns: a bare name (`age`), `$"first name"` for a name that is not an identifier, or
//!   `$3` (1-based, as fields are);
//! * literals: numbers, `"strings"` or `'strings'`, and `/regex/` after `~` / `!~`;
//! * parameters: `@name`, bound to the extra arguments of the call
//!   (`df_filter(d, "age > @min", "min", 30)`): values are passed as data, never spliced into
//!   the text of the expression;
//! * operators, from the lowest precedence: `?:`, `||`, `&&`, `in`, `~ !~`, `< <= == != > >=`,
//!   concatenation (juxtaposition), `+ -`, `* / %`, unary `! - +`, `^ **`;
//!   `x in ("a", "b")` tests membership in a list;
//! * functions: `length substr index tolower toupper lower upper trim ltrim rtrim int abs round
//!   floor ceil sqrt exp log starts_with ends_with contains is_null coalesce min max number string`.
//!
//! Comparisons follow awk's strnum rules (see `column::awk_cmp`), regexes use zawk's regex
//! dialect, and the string functions call the runtime functions behind the awk builtins, so an
//! expression computes what the same awk expression computes on the fields of a record.
use std::collections::HashMap;
use std::sync::Arc;

use super::column::{Value, awk_cmp};
use super::DataFrame;
use crate::runtime::awk_regex::{self, AwkRegex};
use crate::runtime::{Float, Int, Str};

pub(crate) type Res<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    Num(Float, bool),
    Str(Vec<u8>),
    Regex(Vec<u8>),
    Ident(String),
    /// `$name` or `$"name"`
    ColName(String),
    /// `$3`
    ColNum(usize),
    /// `@name`
    Param(String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Assign,
    Question,
    Colon,
}

const OPS: &[&str] = &[
    "&&", "||", "==", "!=", "<=", ">=", "!~", "**", "<", ">", "+", "-", "*", "/", "%", "^", "!",
    "~",
];

/// Splits `src` into tokens, with the byte offset of each.
pub(crate) fn lex(src: &str) -> Res<Vec<(Tok, usize)>> {
    let bs = src.as_bytes();
    let mut toks: Vec<(Tok, usize)> = Vec::new();
    let mut i = 0;
    while i < bs.len() {
        let c = bs[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        // A `/` starts a regex where an operand is expected, and is a division after one.
        let operand_expected = match toks.last() {
            None => true,
            Some((t, _)) => matches!(
                t,
                Tok::Op(_) | Tok::LParen | Tok::Comma | Tok::Assign | Tok::Question | Tok::Colon
            ),
        };
        let tok = match c {
            b'"' | b'\'' => {
                let (s, next) = string_lit(bs, i, c)?;
                i = next;
                Tok::Str(s)
            }
            b'/' if operand_expected => {
                let mut j = i + 1;
                let mut re = Vec::new();
                while j < bs.len() && bs[j] != b'/' {
                    if bs[j] == b'\\' && j + 1 < bs.len() && bs[j + 1] == b'/' {
                        re.push(b'/');
                        j += 2;
                        continue;
                    }
                    re.push(bs[j]);
                    j += 1;
                }
                if j >= bs.len() {
                    return Err(format!("unterminated regex at position {}", start + 1));
                }
                i = j + 1;
                Tok::Regex(re)
            }
            b'0'..=b'9' | b'.' if c != b'.' || bs.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let mut j = i;
                let mut int = true;
                while j < bs.len() && bs[j].is_ascii_digit() {
                    j += 1;
                }
                if j < bs.len() && bs[j] == b'.' {
                    int = false;
                    j += 1;
                    while j < bs.len() && bs[j].is_ascii_digit() {
                        j += 1;
                    }
                }
                if j < bs.len() && (bs[j] == b'e' || bs[j] == b'E') {
                    let mut k = j + 1;
                    if k < bs.len() && (bs[k] == b'+' || bs[k] == b'-') {
                        k += 1;
                    }
                    if k < bs.len() && bs[k].is_ascii_digit() {
                        int = false;
                        j = k;
                        while j < bs.len() && bs[j].is_ascii_digit() {
                            j += 1;
                        }
                    }
                }
                let text = &src[i..j];
                let n: Float = text
                    .parse()
                    .map_err(|_| format!("invalid number `{}`", text))?;
                i = j;
                Tok::Num(n, int && n.abs() < 9.0e15)
            }
            b'$' => {
                i += 1;
                match bs.get(i) {
                    Some(b'"') | Some(b'\'') => {
                        let (s, next) = string_lit(bs, i, bs[i])?;
                        i = next;
                        Tok::ColName(String::from_utf8_lossy(&s).into_owned())
                    }
                    Some(d) if d.is_ascii_digit() => {
                        let mut j = i;
                        while j < bs.len() && bs[j].is_ascii_digit() {
                            j += 1;
                        }
                        let n = src[i..j].parse().unwrap_or(0);
                        i = j;
                        Tok::ColNum(n)
                    }
                    Some(d) if is_ident_start(*d) => {
                        let j = ident_end(bs, i);
                        let name = src[i..j].to_string();
                        i = j;
                        Tok::ColName(name)
                    }
                    _ => return Err(format!("expected a column after `$` at position {}", start + 1)),
                }
            }
            b'@' => {
                let j = ident_end(bs, i + 1);
                if j == i + 1 {
                    return Err(format!("expected a name after `@` at position {}", start + 1));
                }
                let name = src[i + 1..j].to_string();
                i = j;
                Tok::Param(name)
            }
            b'(' => {
                i += 1;
                Tok::LParen
            }
            b')' => {
                i += 1;
                Tok::RParen
            }
            b',' => {
                i += 1;
                Tok::Comma
            }
            b'?' => {
                i += 1;
                Tok::Question
            }
            b':' => {
                i += 1;
                Tok::Colon
            }
            b'=' if bs.get(i + 1) != Some(&b'=') => {
                i += 1;
                Tok::Assign
            }
            c if is_ident_start(c) => {
                let j = ident_end(bs, i);
                let name = src[i..j].to_string();
                i = j;
                Tok::Ident(name)
            }
            _ => {
                let rest = &src[i..];
                match OPS.iter().find(|op| rest.starts_with(**op)) {
                    Some(op) => {
                        i += op.len();
                        Tok::Op(*op)
                    }
                    None => {
                        let ch = rest.chars().next().unwrap_or('?');
                        return Err(format!(
                            "unexpected character `{}` at position {}",
                            ch,
                            start + 1
                        ));
                    }
                }
            }
        };
        toks.push((tok, start));
    }
    Ok(toks)
}

fn is_ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn ident_end(bs: &[u8], mut i: usize) -> usize {
    while i < bs.len() && (bs[i].is_ascii_alphanumeric() || bs[i] == b'_' || bs[i] >= 0x80) {
        i += 1;
    }
    i
}

/// A string literal starting at `bs[i]` (the quote `q`): its bytes and the position after it.
fn string_lit(bs: &[u8], i: usize, q: u8) -> Res<(Vec<u8>, usize)> {
    let mut j = i + 1;
    let mut out = Vec::new();
    while j < bs.len() {
        match bs[j] {
            c if c == q => return Ok((out, j + 1)),
            b'\\' if j + 1 < bs.len() => {
                j += 1;
                out.push(match bs[j] {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    c => c,
                });
            }
            c => out.push(c),
        }
        j += 1;
    }
    Err(format!("unterminated string at position {}", i + 1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Concat,
    Lt,
    Le,
    Eq,
    Ne,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
pub(crate) enum Expr {
    Lit(Value),
    /// A column by name, before `bind`.
    ColName(String),
    /// A column by 1-based number, before `bind`.
    ColNum(usize),
    /// A column by index, after `bind`.
    Col(usize),
    Param(String),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Pos(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `l ~ r`, or `l !~ r` when negated.
    Match(bool, Box<Expr>, Box<Expr>),
    In(Box<Expr>, Vec<Expr>),
    /// `(a, b, c)`: only after `in`.
    List(Vec<Expr>),
    Call(String, Vec<Expr>),
}

/// A recursive descent (Pratt) parser over the tokens of an expression.
pub(crate) struct Parser<'s> {
    src: &'s str,
    toks: Vec<(Tok, usize)>,
    pos: usize,
}

impl<'s> Parser<'s> {
    pub(crate) fn new(src: &'s str) -> Res<Parser<'s>> {
        Ok(Parser {
            src,
            toks: lex(src)?,
            pos: 0,
        })
    }

    pub(crate) fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|(t, _)| t)
    }

    pub(crate) fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|(t, _)| t.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// An error at the current token.
    pub(crate) fn error(&self, msg: &str) -> String {
        match self.toks.get(self.pos) {
            Some((_, at)) => format!("{} at position {} of `{}`", msg, at + 1, self.src),
            None => format!("{} at the end of `{}`", msg, self.src),
        }
    }

    pub(crate) fn expect(&mut self, tok: Tok, what: &str) -> Res<()> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.error(&format!("expected {}", what)))
        }
    }

    /// Whether the current token is the identifier `word` (case-insensitive), consuming it.
    pub(crate) fn eat_word(&mut self, word: &str) -> bool {
        if let Some(Tok::Ident(w)) = self.peek()
            && w.eq_ignore_ascii_case(word)
        {
            self.pos += 1;
            return true;
        }
        false
    }

    pub(crate) fn eat(&mut self, tok: &Tok) -> bool {
        if self.peek() == Some(tok) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// A whole expression: everything up to the end of the input.
    pub(crate) fn parse_all(mut self) -> Res<Expr> {
        let e = self.parse_expr(0)?;
        if !self.at_end() {
            return Err(self.error("unexpected token"));
        }
        Ok(e)
    }

    /// The binding powers (left, right) of the infix operator at the current token.
    fn infix_bp(&self) -> Option<(u8, u8)> {
        Some(match self.peek()? {
            Tok::Question => (2, 1),
            Tok::Op("||") => (3, 4),
            Tok::Op("&&") => (5, 6),
            Tok::Ident(w) if w == "in" => (7, 8),
            Tok::Op("~") | Tok::Op("!~") => (9, 10),
            Tok::Op("<" | "<=" | "==" | "!=" | ">" | ">=") => (11, 12),
            Tok::Op("+" | "-") => (15, 16),
            Tok::Op("*" | "/" | "%") => (17, 18),
            Tok::Op("^" | "**") => (22, 21),
            // Concatenation: an operand right after an operand.
            Tok::Num(..)
            | Tok::Str(_)
            | Tok::Regex(_)
            | Tok::ColName(_)
            | Tok::ColNum(_)
            | Tok::Param(_)
            | Tok::LParen => (13, 14),
            Tok::Ident(w) if !matches!(w.as_str(), "in" | "as" | "asc" | "desc") => (13, 14),
            _ => return None,
        })
    }

    pub(crate) fn parse_expr(&mut self, min_bp: u8) -> Res<Expr> {
        let mut lhs = self.parse_prefix()?;
        while let Some((lbp, rbp)) = self.infix_bp() {
            if lbp < min_bp {
                break;
            }
            let concat = matches!(
                self.peek(),
                Some(
                    Tok::Num(..)
                        | Tok::Str(_)
                        | Tok::Regex(_)
                        | Tok::ColName(_)
                        | Tok::ColNum(_)
                        | Tok::Param(_)
                        | Tok::LParen
                        | Tok::Ident(_)
                )
            ) && !matches!(self.peek(), Some(Tok::Ident(w)) if w == "in");
            if concat {
                let rhs = self.parse_expr(rbp)?;
                lhs = Expr::Bin(BinOp::Concat, Box::new(lhs), Box::new(rhs));
                continue;
            }
            let tok = self.next().expect("infix operator");
            lhs = match tok {
                Tok::Question => {
                    let then = self.parse_expr(0)?;
                    self.expect(Tok::Colon, "`:`")?;
                    let els = self.parse_expr(rbp)?;
                    Expr::Cond(Box::new(lhs), Box::new(then), Box::new(els))
                }
                Tok::Ident(_) => {
                    // `in`
                    let rhs = self.parse_expr(rbp)?;
                    let items = match rhs {
                        Expr::List(items) => items,
                        e => vec![e],
                    };
                    Expr::In(Box::new(lhs), items)
                }
                Tok::Op(op) => {
                    let rhs = self.parse_expr(rbp)?;
                    let (l, r) = (Box::new(lhs), Box::new(rhs));
                    match op {
                        "||" => Expr::Or(l, r),
                        "&&" => Expr::And(l, r),
                        "~" => Expr::Match(false, l, r),
                        "!~" => Expr::Match(true, l, r),
                        _ => {
                            let op = match op {
                                "<" => BinOp::Lt,
                                "<=" => BinOp::Le,
                                "==" => BinOp::Eq,
                                "!=" => BinOp::Ne,
                                ">" => BinOp::Gt,
                                ">=" => BinOp::Ge,
                                "+" => BinOp::Add,
                                "-" => BinOp::Sub,
                                "*" => BinOp::Mul,
                                "/" => BinOp::Div,
                                "%" => BinOp::Mod,
                                _ => BinOp::Pow,
                            };
                            Expr::Bin(op, l, r)
                        }
                    }
                }
                _ => return Err(self.error("unexpected token")),
            };
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> Res<Expr> {
        let Some(tok) = self.next() else {
            return Err(self.error("expected an expression"));
        };
        Ok(match tok {
            Tok::Num(n, true) => Expr::Lit(Value::Int(n as Int)),
            Tok::Num(n, false) => Expr::Lit(Value::Float(n)),
            Tok::Str(s) | Tok::Regex(s) => Expr::Lit(Value::str(s)),
            Tok::ColName(name) => Expr::ColName(name),
            Tok::ColNum(n) => Expr::ColNum(n),
            Tok::Param(name) => Expr::Param(name),
            Tok::Op("!") => Expr::Not(Box::new(self.parse_expr(19)?)),
            Tok::Op("-") => Expr::Neg(Box::new(self.parse_expr(19)?)),
            Tok::Op("+") => Expr::Pos(Box::new(self.parse_expr(19)?)),
            Tok::LParen => {
                let mut items = vec![self.parse_expr(0)?];
                while self.eat(&Tok::Comma) {
                    items.push(self.parse_expr(0)?);
                }
                self.expect(Tok::RParen, "`)`")?;
                if items.len() == 1 {
                    items.pop().unwrap()
                } else {
                    Expr::List(items)
                }
            }
            Tok::Ident(name) => {
                if self.eat(&Tok::LParen) {
                    let mut args = Vec::new();
                    if !self.eat(&Tok::RParen) {
                        loop {
                            // `count(*)`
                            if self.peek() == Some(&Tok::Op("*")) {
                                self.pos += 1;
                            } else {
                                args.push(self.parse_expr(0)?);
                            }
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            self.expect(Tok::RParen, "`)`")?;
                            break;
                        }
                    }
                    Expr::Call(name, args)
                } else {
                    match name.as_str() {
                        "true" => Expr::Lit(Value::Int(1)),
                        "false" => Expr::Lit(Value::Int(0)),
                        "null" => Expr::Lit(Value::Null),
                        _ => Expr::ColName(name),
                    }
                }
            }
            _ => {
                self.pos -= 1;
                return Err(self.error("expected an expression"));
            }
        })
    }
}

/// The scalar functions of expressions, with their least and most numbers of arguments.
const FUNCS: &[(&str, usize, usize)] = &[
    ("length", 1, 1),
    ("substr", 2, 3),
    ("index", 2, 2),
    ("tolower", 1, 1),
    ("toupper", 1, 1),
    ("lower", 1, 1),
    ("upper", 1, 1),
    ("trim", 1, 1),
    ("ltrim", 1, 1),
    ("rtrim", 1, 1),
    ("int", 1, 1),
    ("abs", 1, 1),
    ("round", 1, 2),
    ("floor", 1, 1),
    ("ceil", 1, 1),
    ("sqrt", 1, 1),
    ("exp", 1, 1),
    ("log", 1, 1),
    ("starts_with", 2, 2),
    ("ends_with", 2, 2),
    ("contains", 2, 2),
    ("is_null", 1, 1),
    ("isnull", 1, 1),
    ("coalesce", 1, usize::MAX),
    ("min", 1, usize::MAX),
    ("max", 1, usize::MAX),
    ("number", 1, 1),
    ("string", 1, 1),
];

/// The index of the column `name` of `names`, or an error suggesting a close name.
pub(crate) fn resolve_name(names: &[&str], name: &str) -> Res<usize> {
    if let Some(i) = names.iter().position(|n| *n == name) {
        return Ok(i);
    }
    // a column number, as for `$3`
    if let Ok(n) = name.trim_start_matches('$').parse::<usize>()
        && n >= 1
        && n <= names.len()
    {
        return Ok(n - 1);
    }
    let best = names
        .iter()
        .map(|n| (strsim::jaro_winkler(n, name), *n))
        .filter(|(score, _)| *score >= 0.8)
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Err(match best {
        Some((_, n)) => format!("unknown column \"{}\" (did you mean \"{}\"?)", name, n),
        None => format!(
            "unknown column \"{}\" (columns: {})",
            name,
            if names.is_empty() {
                "none".to_string()
            } else {
                names.join(", ")
            }
        ),
    })
}

impl Expr {
    /// Resolves the columns and parameters of the expression against `df` and `params`, and
    /// checks the function calls.
    pub(crate) fn bind(&mut self, df: &DataFrame, params: &HashMap<String, Value>) -> Res<()> {
        match self {
            Expr::Lit(_) | Expr::Col(_) => {}
            Expr::ColName(name) => {
                let names = df.names();
                *self = Expr::Col(resolve_name(&names, name)?);
            }
            Expr::ColNum(n) => {
                if *n < 1 || *n > df.columns.len() {
                    return Err(format!(
                        "column ${} out of range (the table has {} columns)",
                        n,
                        df.columns.len()
                    ));
                }
                *self = Expr::Col(*n - 1);
            }
            Expr::Param(name) => match params.get(name.as_str()) {
                Some(v) => *self = Expr::Lit(v.clone()),
                None => return Err(format!("parameter @{} is not given", name)),
            },
            Expr::Not(e) | Expr::Neg(e) | Expr::Pos(e) => e.bind(df, params)?,
            Expr::Bin(_, l, r) | Expr::And(l, r) | Expr::Or(l, r) | Expr::Match(_, l, r) => {
                l.bind(df, params)?;
                r.bind(df, params)?;
            }
            Expr::Cond(c, t, e) => {
                c.bind(df, params)?;
                t.bind(df, params)?;
                e.bind(df, params)?;
            }
            Expr::In(l, items) => {
                l.bind(df, params)?;
                for i in items {
                    i.bind(df, params)?;
                }
            }
            Expr::List(_) => return Err("a list `(a, b)` is only valid after `in`".to_string()),
            Expr::Call(name, args) => {
                let Some((_, min, max)) = FUNCS.iter().find(|(n, _, _)| *n == name.as_str())
                else {
                    return Err(format!("unknown function `{}` in an expression", name));
                };
                if args.len() < *min || args.len() > *max {
                    return Err(format!(
                        "function `{}` expects {} arguments, got {}",
                        name,
                        if min == max {
                            min.to_string()
                        } else if *max == usize::MAX {
                            format!("at least {}", min)
                        } else {
                            format!("{} to {}", min, max)
                        },
                        args.len()
                    ));
                }
                for a in args {
                    a.bind(df, params)?;
                }
            }
        }
        Ok(())
    }

    /// The column names an expression refers to, before `bind` (for the default names of
    /// aggregates: `sum(amount)` is `sum_amount`).
    pub(crate) fn column_hint(&self) -> Option<String> {
        match self {
            Expr::ColName(n) => Some(n.clone()),
            Expr::ColNum(n) => Some(format!("c{}", n)),
            _ => None,
        }
    }
}

/// State kept while an expression is evaluated over the rows of a table: the compiled regexes.
#[derive(Default)]
pub(crate) struct EvalCtx {
    regexes: HashMap<Vec<u8>, Arc<AwkRegex>>,
}

impl EvalCtx {
    fn regex(&mut self, pat: &[u8]) -> Res<Arc<AwkRegex>> {
        if let Some(re) = self.regexes.get(pat) {
            return Ok(re.clone());
        }
        let text = String::from_utf8_lossy(pat);
        let re = awk_regex::compile(&text)
            .map_err(|e| awk_regex::compile_error(&text, &e))?;
        let re = Arc::new(re);
        if self.regexes.len() > 1024 {
            self.regexes.clear();
        }
        self.regexes.insert(pat.to_vec(), re.clone());
        Ok(re)
    }
}

fn bool_val(b: bool) -> Value {
    Value::Int(b as Int)
}

fn str_of(v: &Value) -> Str<'static> {
    Str::from_bytes(&v.text(false))
}

fn str_val(s: Str) -> Value {
    Value::str(s.with_bytes(|bs| bs.to_vec()))
}

impl Expr {
    /// The value of a bound expression for row `row` of `df`.
    pub(crate) fn eval(&self, df: &DataFrame, row: usize, ctx: &mut EvalCtx) -> Res<Value> {
        Ok(match self {
            Expr::Lit(v) => v.clone(),
            Expr::Col(i) => df.columns[*i].get(row),
            Expr::ColName(_) | Expr::ColNum(_) | Expr::Param(_) | Expr::List(_) => {
                return Err("internal error: unbound expression".to_string());
            }
            Expr::Not(e) => bool_val(!e.eval(df, row, ctx)?.truthy()),
            Expr::Neg(e) => match e.eval(df, row, ctx)? {
                Value::Int(i) if i != Int::MIN => Value::Int(-i),
                v => Value::num(-v.to_num()),
            },
            Expr::Pos(e) => match e.eval(df, row, ctx)? {
                v @ (Value::Int(_) | Value::Float(_)) => v,
                v => Value::num(v.to_num()),
            },
            Expr::And(l, r) => {
                bool_val(l.eval(df, row, ctx)?.truthy() && r.eval(df, row, ctx)?.truthy())
            }
            Expr::Or(l, r) => {
                bool_val(l.eval(df, row, ctx)?.truthy() || r.eval(df, row, ctx)?.truthy())
            }
            Expr::Cond(c, t, e) => {
                if c.eval(df, row, ctx)?.truthy() {
                    t.eval(df, row, ctx)?
                } else {
                    e.eval(df, row, ctx)?
                }
            }
            Expr::Match(negated, l, r) => {
                let hay = l.eval(df, row, ctx)?;
                let pat = r.eval(df, row, ctx)?;
                let re = ctx.regex(&pat.text(false))?;
                bool_val(re.is_match(&hay.text(false)) != *negated)
            }
            Expr::In(l, items) => {
                let v = l.eval(df, row, ctx)?;
                let mut found = false;
                for item in items {
                    if awk_cmp(&v, &item.eval(df, row, ctx)?) == std::cmp::Ordering::Equal {
                        found = true;
                        break;
                    }
                }
                bool_val(found)
            }
            Expr::Bin(op, l, r) => {
                let l = l.eval(df, row, ctx)?;
                let r = r.eval(df, row, ctx)?;
                binop(*op, &l, &r)?
            }
            Expr::Call(name, args) => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(a.eval(df, row, ctx)?);
                }
                call(name, &vals)
            }
        })
    }
}

fn binop(op: BinOp, l: &Value, r: &Value) -> Res<Value> {
    use std::cmp::Ordering::*;
    Ok(match op {
        BinOp::Lt => bool_val(awk_cmp(l, r) == Less),
        BinOp::Le => bool_val(awk_cmp(l, r) != Greater),
        BinOp::Eq => bool_val(awk_cmp(l, r) == Equal),
        BinOp::Ne => bool_val(awk_cmp(l, r) != Equal),
        BinOp::Gt => bool_val(awk_cmp(l, r) == Greater),
        BinOp::Ge => bool_val(awk_cmp(l, r) != Less),
        BinOp::Concat => {
            let mut s = l.text(false).into_owned();
            s.extend_from_slice(&r.text(false));
            Value::str(s)
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul => {
            if let (Value::Int(x), Value::Int(y)) = (l, r) {
                let exact = match op {
                    BinOp::Add => x.checked_add(*y),
                    BinOp::Sub => x.checked_sub(*y),
                    _ => x.checked_mul(*y),
                };
                if let Some(v) = exact {
                    return Ok(Value::Int(v));
                }
            }
            let (x, y) = (l.to_num(), r.to_num());
            Value::num(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                _ => x * y,
            })
        }
        BinOp::Div => {
            let y = r.to_num();
            if y == 0.0 {
                return Err("division by zero".to_string());
            }
            Value::num(l.to_num() / y)
        }
        BinOp::Mod => {
            let y = r.to_num();
            if y == 0.0 {
                return Err("division by zero in %".to_string());
            }
            Value::num(l.to_num() % y)
        }
        BinOp::Pow => Value::num(l.to_num().powf(r.to_num())),
    })
}

/// A call of the scalar function `name` (checked by `bind`).
fn call(name: &str, args: &[Value]) -> Value {
    let num = |i: usize| args[i].to_num();
    match name {
        "length" => Value::Int(str_of(&args[0]).len() as Int),
        "substr" => {
            let s = str_of(&args[0]);
            let len = s.len() as Float;
            let start = num(1).round();
            let end = if args.len() > 2 {
                start + num(2).round()
            } else {
                Float::INFINITY
            };
            let from = start.max(1.0);
            let to = end.min(len + 1.0);
            if !(to > from) {
                return Value::str("");
            }
            str_val(s.sub_str(from as usize - 1, (to - from) as usize))
        }
        "index" => Value::Int(crate::runtime::string_search::index_substr(
            &str_of(&args[1]),
            &str_of(&args[0]),
        )),
        "tolower" | "lower" => str_val(str_of(&args[0]).to_lower_ascii()),
        "toupper" | "upper" => str_val(str_of(&args[0]).to_upper_ascii()),
        "trim" | "ltrim" | "rtrim" => {
            let text = args[0].text(false);
            let bs: &[u8] = &text;
            let bs = match name {
                "trim" => bs.trim_ascii(),
                "ltrim" => bs.trim_ascii_start(),
                _ => bs.trim_ascii_end(),
            };
            Value::str(bs.to_vec())
        }
        "int" => Value::num(num(0).trunc()),
        "abs" => match &args[0] {
            Value::Int(i) if *i != Int::MIN => Value::Int(i.abs()),
            v => Value::num(v.to_num().abs()),
        },
        "round" => {
            let digits = if args.len() > 1 { num(1) } else { 0.0 };
            Value::num(crate::builtins::round_digits(num(0), digits))
        }
        "floor" => Value::num(num(0).floor()),
        "ceil" => Value::num(num(0).ceil()),
        "sqrt" => Value::num(num(0).sqrt()),
        "exp" => Value::num(num(0).exp()),
        "log" => Value::num(num(0).ln()),
        "starts_with" => bool_val(args[0].text(false).starts_with(&args[1].text(false))),
        "ends_with" => bool_val(args[0].text(false).ends_with(&args[1].text(false))),
        "contains" => {
            let hay = args[0].text(false);
            let needle = args[1].text(false);
            bool_val(
                needle.is_empty() || hay.windows(needle.len()).any(|w| w == &needle[..]),
            )
        }
        "is_null" | "isnull" => bool_val(args[0].is_missing()),
        "coalesce" => args
            .iter()
            .find(|v| !v.is_missing())
            .cloned()
            .unwrap_or(Value::Null),
        "min" | "max" => {
            let mut best: Option<&Value> = None;
            for v in args.iter().filter(|v| !v.is_missing()) {
                best = match best {
                    None => Some(v),
                    Some(b) => {
                        let ord = awk_cmp(v, b);
                        let better = if name == "min" {
                            ord == std::cmp::Ordering::Less
                        } else {
                            ord == std::cmp::Ordering::Greater
                        };
                        Some(if better { v } else { b })
                    }
                };
            }
            best.cloned().unwrap_or(Value::Null)
        }
        "number" => Value::num(num(0)),
        "string" => Value::str(args[0].text(false).into_owned()),
        _ => Value::Null,
    }
}

thread_local! {
    /// Parsed expressions by their text: an expression in a call made for each record is
    /// parsed once.
    static PARSED: std::cell::RefCell<HashMap<String, Expr>> = std::cell::RefCell::new(HashMap::new());
}

/// The parsed (unbound) expression `src`, from the cache when it was parsed before.
pub(crate) fn parse_cached(src: &str) -> Res<Expr> {
    if let Some(e) = PARSED.with(|p| p.borrow().get(src).cloned()) {
        return Ok(e);
    }
    let e = Parser::new(src)?.parse_all()?;
    PARSED.with(|p| {
        let mut p = p.borrow_mut();
        if p.len() >= 256 {
            p.clear();
        }
        p.insert(src.to_string(), e.clone());
    });
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::df::column::Column;

    fn frame() -> DataFrame {
        let mut df = DataFrame::default();
        df.add_column(Column::from_values(
            "name",
            None,
            vec![Value::input("Ann"), Value::input("Bob"), Value::input("")],
        ));
        df.add_column(Column::from_values(
            "age",
            None,
            vec![Value::input("10"), Value::input("9"), Value::input("")],
        ));
        df
    }

    fn eval_all(src: &str) -> Vec<String> {
        let df = frame();
        let mut e = Parser::new(src).unwrap().parse_all().unwrap();
        e.bind(&df, &HashMap::new()).unwrap();
        let mut ctx = EvalCtx::default();
        (0..df.nrows)
            .map(|i| e.eval(&df, i, &mut ctx).unwrap().text_lossy(true))
            .collect()
    }

    #[test]
    fn strnum_semantics() {
        // "10" > "9" numerically, as `$2 > 9` in awk
        assert_eq!(eval_all("age > 9"), ["1", "0", "0"]);
        // "" < 1 is true, as in awk
        assert_eq!(eval_all("age < 1"), ["0", "0", "1"]);
        // a string constant compares as a string
        assert_eq!(eval_all("age < \"9\""), ["1", "0", "1"]);
    }

    #[test]
    fn operators() {
        assert_eq!(eval_all("age * 2 + 1"), ["21", "19", "1"]);
        assert_eq!(eval_all("name \"-\" age"), ["Ann-10", "Bob-9", "-"]);
        assert_eq!(eval_all("name ~ /^A/ ? 'a' : 'b'"), ["a", "b", "b"]);
        assert_eq!(eval_all("name in ('Bob', 'Cy')"), ["0", "1", "0"]);
        assert_eq!(eval_all("-2 ^ 2"), ["-4", "-4", "-4"]);
        assert_eq!(eval_all("!is_null(name) && length(name) == 3"), ["1", "1", "0"]);
        assert_eq!(eval_all("upper(substr(name, 2))"), ["NN", "OB", ""]);
        assert_eq!(eval_all("$2 + 0"), ["10", "9", "0"]);
    }

    #[test]
    fn errors() {
        let df = frame();
        let mut e = Parser::new("agee > 1").unwrap().parse_all().unwrap();
        let err = e.bind(&df, &HashMap::new()).unwrap_err();
        assert!(err.contains("did you mean \"age\""), "{}", err);
        assert!(Parser::new("age >").unwrap().parse_all().is_err());
        assert!(Parser::new("'abc").is_err());
    }
}
