//! `zawk fmt`: formats AWK source code.
//!
//! The formatter works on the tokens of the lexer rather than on the syntax tree: the tree drops
//! comments and desugars the program (`!=`, `in`, f-strings, multi-dimensional keys, the text of
//! literals...), so a program printed back from it would not be the program that was written. It
//! indents blocks, puts one statement per line, normalizes the spaces between tokens (`if (`,
//! `a = b + 1`, `f(x, y)`, `$NF`, `i++`), joins `}` with a following `else` and keeps comments,
//! blank lines (at most one in a row) and the line breaks inside expressions.
//!
//! The formatted program is checked to parse to the same syntax tree as the original one, so a
//! formatter bug can not silently change the meaning of a program.
use crate::arena::Arena;
use crate::ast;
use crate::common::Stage;
use crate::lexer::{Tok, Tokenizer};
use crate::parsing;

pub struct Options {
    /// The text of one level of indentation: spaces or a tab.
    pub indent: String,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            indent: "    ".into(),
        }
    }
}

/// Formats the program `text`, or returns why it can not be formatted (a syntax error).
pub fn format(text: &str, opts: &Options) -> Result<String, String> {
    let before = syntax_tree(text)?;
    let (toks, end) = tokens(text)?;
    let mut f = Formatter::new(&opts.indent);
    for i in 0..toks.len() {
        f.gap(&toks, i);
        f.token(&toks, i);
    }
    f.trailing_gap(&toks, &text[end..]);
    let mut out = f.out;
    if !out.is_empty() {
        out.push('\n');
    }
    match syntax_tree(&out) {
        Ok(after) if after == before => Ok(out),
        _ => Err("internal error: the formatted program does not mean the same as the original one; please report it".into()),
    }
}

/// A description of the syntax tree of a program, to compare two programs.
fn syntax_tree(text: &str) -> Result<String, String> {
    let a = Arena::default();
    let text: &str = a.alloc_str(text);
    let mut buf = Vec::new();
    let mut prog = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    if let Err(e) = parser.parse(&a, &mut buf, &mut prog, Tokenizer::new(text)) {
        let (loc, desc) = parsing::describe_parse_error(text, &e);
        return Err(format!("syntax error at {}: {}", loc, desc));
    }
    let mut res = String::new();
    for dec in prog.decs.iter() {
        res.push_str(&format!(
            "function {} {:?} {}\n",
            dec.name,
            dec.args,
            stmt(dec.body)
        ));
    }
    for (name, blocks) in [
        ("BEGIN", &prog.begin),
        ("PREPARE", &prog.prepare),
        ("BEGINFILE", &prog.beginfile),
        ("ENDFILE", &prog.endfile),
        ("END", &prog.end),
    ] {
        for b in blocks.iter() {
            res.push_str(&format!("{} {}\n", name, stmt(b)));
        }
    }
    for (pat, body) in prog.pats.iter() {
        res.push_str(&format!("{:?} {}\n", pat, body.map_or(String::new(), stmt)));
    }
    Ok(res)
}

/// A description of a statement, where an empty statement in a block (`x;;`, or the one the lexer
/// adds in `{ x }}`) is left out, and a block of one statement is that statement.
fn stmt(s: &ast::Stmt<&str>) -> String {
    use ast::Stmt::*;
    match s {
        Block(v) => {
            let items: Vec<String> = v
                .iter()
                .filter(|s| !matches!(s, Block(v) if v.is_empty()))
                .map(|s| stmt(s))
                .collect();
            match items.len() {
                1 => items.into_iter().next().unwrap(),
                _ => format!("{{{}}}", items.join("; ")),
            }
        }
        If(c, t, e) => format!(
            "if({:?}) {} else {}",
            c,
            stmt(t),
            e.map_or(String::new(), stmt)
        ),
        For(i, c, u, b) => format!(
            "for({}; {:?}; {}) {}",
            i.map_or(String::new(), stmt),
            c,
            u.map_or(String::new(), stmt),
            stmt(b)
        ),
        DoWhile(c, b) => format!("do {} while({:?})", stmt(b), c),
        While(top, c, b) => format!("while{}({:?}) {}", top, c, stmt(b)),
        ForEach(v, a, b) => format!("for({} in {:?}) {}", v, a, stmt(b)),
        ForRange(v, s, e, inclusive, b) => {
            format!(
                "for({} in {:?}{}{:?}) {}",
                v,
                s,
                if *inclusive { "..=" } else { ".." },
                e,
                stmt(b)
            )
        }
        Switch(e, cases) => {
            let cases: Vec<String> = cases
                .iter()
                .map(|(l, b)| format!("{:?}: {}", l, stmt(b)))
                .collect();
            format!("switch({:?}) {{{}}}", e, cases.join("; "))
        }
        EndBlock(b) => format!("end {}", stmt(b)),
        s => format!("{:?}", s),
    }
}

struct Token<'a> {
    tok: Tok<'a>,
    /// The text of the token in the program.
    text: &'a str,
    /// The text between the previous token and this one: blanks, comments and line continuations.
    gap: &'a str,
    /// The token is the first of the program, or the token before it is a newline.
    after_newline: bool,
}

/// The tokens of the program, without the `;` the lexer adds before a `}` and the newline it adds
/// at the end (they have no text), and the offset just past the last token.
fn tokens(text: &str) -> Result<(Vec<Token<'_>>, usize), String> {
    let mut res: Vec<Token> = Vec::new();
    let mut end = 0;
    for t in Tokenizer::new(text) {
        let (l, tok, r) = t.map_err(|e| format!("syntax error at {}", e))?;
        if l.offset() == r.offset() {
            continue;
        }
        let after_newline = res.last().map_or(true, |t| t.tok == Tok::Newline);
        res.push(Token {
            tok,
            text: &text[l.offset()..r.offset()],
            gap: &text[end..l.offset()],
            after_newline,
        });
        end = r.offset();
    }
    Ok((res, end))
}

enum GapItem<'a> {
    Comment(&'a str),
    /// A backslash before a newline.
    Continuation,
}

fn gap_items(gap: &str) -> Vec<GapItem<'_>> {
    let mut res = Vec::new();
    let mut rest = gap;
    while let Some(c) = rest.chars().next() {
        if c == '#' {
            let end = rest.find('\n').unwrap_or(rest.len());
            res.push(GapItem::Comment(rest[..end].trim_end()));
            rest = &rest[end..];
        } else if rest.starts_with("\\\n") || rest.starts_with("\\\r\n") {
            res.push(GapItem::Continuation);
            rest = &rest[rest.find('\n').unwrap() + 1..];
        } else {
            rest = &rest[c.len_utf8()..];
        }
    }
    res
}

/// The keyword before the parenthesis of a statement (or of a function declaration): after the
/// `)`, a newline before the `{` of the body is dropped, and a body on its own line is indented.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Head {
    If,
    While,
    For,
    Switch,
    Function,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Block,
    /// The body of a switch: the case labels are indented one level less than the statements.
    Switch,
    /// The body of a `do` loop, followed by its `while` on the same line.
    Do,
}

struct Frame {
    /// The indentation of the line with the `{` (and of the `}`).
    indent: usize,
    /// The indentation of the statements around the block.
    base: usize,
    kind: BlockKind,
}

struct Formatter<'a, 'u> {
    unit: &'u str,
    out: String,
    /// Nothing has been written on the current line.
    line_empty: bool,
    /// The next token or comment starts a new line.
    want_break: bool,
    /// That line break is a line continuation (a backslash at the end of the line).
    backslash: bool,
    /// That line continues an expression, and is indented one more level.
    cont: bool,
    /// The number of newlines in the program since the last token or comment.
    newlines: usize,
    /// No blank line follows: the last token is `{`.
    no_blank: bool,
    /// The indentation of the statements of the current block.
    base: usize,
    /// The levels added for bodies on their own line (`if (x)` + newline + `print`).
    hang: usize,
    /// A level has been added for the expected body.
    hung: bool,
    frames: Vec<Frame>,
    /// The open parentheses and brackets, with their statement keyword.
    parens: Vec<Option<Head>>,
    /// The keyword of the last `)`.
    last_closed: Option<Head>,
    /// The keyword of the next `(`.
    pending_head: Option<Head>,
    /// The number of frames when each `do` without its `while` started.
    pending_do: Vec<usize>,
    /// The last token written.
    prev: Option<Tok<'a>>,
    /// The last token ends an operand, so a following `-` is a binary operator and `++` a
    /// postfix one.
    prev_operand: bool,
    /// The last token is a unary operator.
    prev_unary: bool,
    at_stmt_start: bool,
    /// The last token is `else`, `do` or the `)` of a statement keyword: a statement follows.
    expect_body: bool,
    /// Between `case` (or `default`) and its `:`.
    in_label: bool,
    /// A comment has been written since the last token.
    commented: bool,
}

/// Tokens after which a newline continues the expression.
fn continues(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Comma
            | Tok::AND
            | Tok::OR
            | Tok::LParen
            | Tok::LBrack
            | Tok::CallStart(_)
            | Tok::PrintLP
            | Tok::PrintfLP
            | Tok::ExitLP
    )
}

fn is_operand(tok: &Tok) -> bool {
    matches!(
        tok,
        Tok::Ident(_)
            | Tok::StrLit(_)
            | Tok::FStrLit(_)
            | Tok::RawStrLit(_)
            | Tok::PatLit(_)
            | Tok::ILit(_)
            | Tok::HexLit(_)
            | Tok::FLit(_)
            | Tok::RParen
            | Tok::RBrack
    )
}

impl<'a, 'u> Formatter<'a, 'u> {
    fn new(unit: &'u str) -> Self {
        Formatter {
            unit,
            out: String::new(),
            line_empty: true,
            want_break: false,
            backslash: false,
            cont: false,
            newlines: 0,
            no_blank: false,
            base: 0,
            hang: 0,
            hung: false,
            frames: Vec::new(),
            parens: Vec::new(),
            last_closed: None,
            pending_head: None,
            pending_do: Vec::new(),
            prev: None,
            prev_operand: false,
            prev_unary: false,
            at_stmt_start: true,
            expect_body: false,
            in_label: false,
            commented: false,
        }
    }

    /// Writes `text`, on a new line if one is wanted (indented `dedent` levels less), or after a
    /// space if `space` is true.
    fn write(&mut self, text: &str, space: bool, dedent: usize) {
        if self.want_break && !self.out.is_empty() {
            if self.backslash {
                self.out.push_str(" \\");
            }
            self.out.push('\n');
            if self.newlines >= 2 && !self.no_blank {
                self.out.push('\n');
            }
            self.line_empty = true;
        }
        if self.line_empty {
            let level = (self.base + self.hang + self.cont as usize).saturating_sub(dedent);
            for _ in 0..level {
                self.out.push_str(self.unit);
            }
            self.cont = false;
        } else if space {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.line_empty = false;
        self.want_break = false;
        self.backslash = false;
        self.newlines = 0;
        self.no_blank = false;
    }

    fn end_statement(&mut self) {
        self.want_break = true;
        self.hang = 0;
        self.hung = false;
        self.at_stmt_start = true;
        self.expect_body = false;
        self.in_label = false;
    }

    /// The comments and line continuations before token `i`.
    fn gap(&mut self, toks: &[Token<'a>], i: usize) {
        let t = &toks[i];
        for item in gap_items(t.gap) {
            match item {
                GapItem::Comment(c) => self.comment(c, t.after_newline),
                GapItem::Continuation if !self.want_break && !self.out.is_empty() => {
                    self.want_break = true;
                    self.backslash = true;
                    self.cont = true;
                }
                GapItem::Continuation => {}
            }
        }
    }

    /// The comments after the last token.
    fn trailing_gap(&mut self, toks: &[Token<'a>], gap: &str) {
        let after_newline = toks.last().map_or(true, |t| t.tok == Tok::Newline);
        for item in gap_items(gap) {
            if let GapItem::Comment(c) = item {
                self.comment(c, after_newline);
            }
        }
    }

    fn comment(&mut self, text: &str, own_line: bool) {
        if own_line || self.line_empty {
            if !self.line_empty {
                self.want_break = true;
            }
            self.write(text, false, 0);
        } else {
            // A comment at the end of the line, even if a line break is wanted after the last token.
            self.out.push(' ');
            self.out.push_str(text);
            self.backslash = false;
            self.newlines = 0;
        }
        // The comment ends the line.
        self.want_break = true;
        self.commented = true;
    }

    fn next_significant(toks: &[Token<'a>], i: usize) -> Option<Tok<'a>> {
        toks[i + 1..]
            .iter()
            .map(|t| &t.tok)
            .find(|t| **t != Tok::Newline)
            .cloned()
    }

    fn newline(&mut self, toks: &[Token<'a>], i: usize) {
        self.newlines += 1;
        let next = Self::next_significant(toks, i);
        if !self.parens.is_empty() || self.prev.as_ref().is_some_and(continues) {
            self.want_break = true;
            self.cont = true;
            return;
        }
        if self.expect_body {
            // `if (x)` + newline + `{`, `else` + newline + `if`: the newline is dropped.
            let joined = next == Some(Tok::LBrace)
                || (self.prev == Some(Tok::Else) && next == Some(Tok::If));
            if !joined {
                if !self.hung {
                    self.hang += 1;
                    self.hung = true;
                }
                self.want_break = true;
            }
            return;
        }
        if self.prev == Some(Tok::RBrace) && !self.at_stmt_start {
            // `}` + newline + `else` (or the `while` of a `do`).
            return;
        }
        if matches!(
            self.prev,
            Some(Tok::Begin | Tok::End | Tok::Prepare | Tok::BeginFile | Tok::EndFile)
        ) {
            // `BEGIN` + newline + `{`.
            return;
        }
        if self.at_stmt_start {
            self.want_break = true;
        } else {
            self.end_statement();
        }
    }

    /// Writes a token that is not a newline, after a space if `space`, and records it.
    fn emit(&mut self, tok: &Tok<'a>, text: &str, space: bool, dedent: usize) {
        self.write(text, space, dedent);
        self.prev = Some(tok.clone());
        self.prev_operand = is_operand(tok);
        self.prev_unary = false;
        self.at_stmt_start = false;
        self.expect_body = false;
        self.hung = false;
        self.commented = false;
    }

    /// Whether a space goes between the last token and `tok`, a token that has no special rule.
    fn space_before(&self, tok: &Tok) -> bool {
        let Some(prev) = &self.prev else {
            return false;
        };
        if matches!(
            tok,
            Tok::RParen | Tok::RBrack | Tok::LBrack | Tok::Comma | Tok::Semi
        ) {
            return false;
        }
        if self.prev_unary {
            // `- -x` is not `--x`.
            return matches!(
                (prev, tok),
                (Tok::Sub, Tok::Sub | Tok::Decr) | (Tok::Add, Tok::Add | Tok::Incr)
            );
        }
        !matches!(
            prev,
            Tok::LParen
                | Tok::LBrack
                | Tok::CallStart(_)
                | Tok::PrintLP
                | Tok::PrintfLP
                | Tok::ExitLP
                | Tok::Dollar
                | Tok::Not
                | Tok::FunDec(_)
        )
    }

    fn token(&mut self, toks: &[Token<'a>], i: usize) {
        let t = &toks[i];
        let tok = &t.tok;
        let head = self.pending_head.take();
        match tok {
            Tok::Newline => {
                self.pending_head = head;
                self.newline(toks, i);
            }
            Tok::LBrace => {
                let kind = match self.prev {
                    Some(Tok::RParen) if self.last_closed == Some(Head::Switch) => {
                        BlockKind::Switch
                    }
                    Some(Tok::Do) => BlockKind::Do,
                    _ => BlockKind::Block,
                };
                let space = self.prev.is_some();
                self.emit(tok, "{", space, 0);
                let indent = self.base + self.hang;
                self.frames.push(Frame {
                    indent,
                    base: self.base,
                    kind,
                });
                self.base = indent + 1;
                self.hang = 0;
                self.want_break = true;
                self.no_blank = true;
                self.at_stmt_start = true;
            }
            Tok::RBrace => {
                // `{}` stays on one line, unless it holds a comment.
                let empty = self.prev == Some(Tok::LBrace) && !self.commented;
                self.want_break = !empty;
                self.newlines = 0;
                self.cont = false;
                let frame = self.frames.pop().unwrap_or(Frame {
                    indent: 0,
                    base: 0,
                    kind: BlockKind::Block,
                });
                self.base = frame.base;
                self.hang = frame.indent - frame.base;
                self.emit(tok, "}", false, 0);
                match Self::next_significant(toks, i) {
                    Some(Tok::Else) => {}
                    Some(Tok::While) if frame.kind == BlockKind::Do => {}
                    _ => self.end_statement(),
                }
            }
            Tok::Else | Tok::Do => {
                if *tok == Tok::Do {
                    self.pending_do.push(self.frames.len());
                }
                let space = self.space_before(tok);
                self.emit(tok, t.text, space, 0);
                self.expect_body = true;
                self.at_stmt_start = true;
            }
            Tok::If | Tok::For | Tok::Switch | Tok::While => {
                let head = match tok {
                    Tok::If => Some(Head::If),
                    Tok::For => Some(Head::For),
                    Tok::Switch => Some(Head::Switch),
                    // The `while` of a `do` loop is not followed by a body.
                    _ if self.pending_do.last() == Some(&self.frames.len()) => {
                        self.pending_do.pop();
                        None
                    }
                    _ => Some(Head::While),
                };
                let space = self.space_before(tok);
                self.emit(tok, t.text, space, 0);
                self.pending_head = head;
            }
            Tok::FunDec(name) => {
                let keyword = t.text.split_whitespace().next().unwrap_or("function");
                let space = self.space_before(tok);
                self.emit(tok, &format!("{} {}", keyword, name), space, 0);
                self.pending_head = Some(Head::Function);
            }
            Tok::LParen => {
                let space = self.space_before(tok);
                self.emit(tok, "(", space, 0);
                self.parens.push(head);
            }
            Tok::LBrack | Tok::CallStart(_) | Tok::PrintLP | Tok::PrintfLP | Tok::ExitLP => {
                let text = match tok {
                    Tok::PrintLP => "print(",
                    Tok::PrintfLP => "printf(",
                    _ => t.text,
                };
                let space = self.space_before(tok);
                self.emit(tok, text, space, 0);
                self.parens.push(None);
            }
            Tok::RParen | Tok::RBrack => {
                let head = self.parens.pop().flatten();
                self.emit(tok, t.text, false, 0);
                if *tok == Tok::RParen {
                    self.last_closed = head;
                    if head.is_some() {
                        self.expect_body = true;
                        self.at_stmt_start = true;
                    }
                }
            }
            Tok::Semi if !self.parens.is_empty() => self.emit(tok, ";", false, 0),
            Tok::Semi if self.expect_body => {
                // The empty statement, as in `while (x--) ;`.
                let space = self.space_before(tok) || self.prev.is_some();
                self.emit(tok, ";", space, 0);
                self.end_statement();
            }
            Tok::Semi => {
                // A `;` ends the statement, which ends the line; other `;` are dropped.
                if !self.at_stmt_start {
                    self.end_statement();
                }
            }
            Tok::Case | Tok::Default
                if self.parens.is_empty()
                    && self
                        .frames
                        .last()
                        .is_some_and(|f| f.kind == BlockKind::Switch) =>
            {
                self.emit(tok, t.text, false, 1);
                self.in_label = true;
            }
            Tok::COLON if self.in_label && self.parens.is_empty() => {
                self.emit(tok, ":", false, 0);
                self.end_statement();
            }
            Tok::Incr | Tok::Decr if self.prev_operand && !self.at_stmt_start => {
                // A postfix `++`.
                self.emit(tok, t.text, false, 0);
                self.prev_operand = true;
            }
            Tok::Incr | Tok::Decr | Tok::Not | Tok::Dollar => {
                let space = self.space_before(tok);
                self.emit(tok, t.text, space, 0);
                self.prev_unary = !matches!(tok, Tok::Dollar);
            }
            Tok::Sub | Tok::Add if self.at_stmt_start || !self.prev_operand => {
                // A unary `-`.
                let space = self.space_before(tok);
                self.emit(tok, t.text, space, 0);
                self.prev_unary = true;
            }
            _ => {
                let space = self.space_before(tok);
                self.emit(tok, t.text, space, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(text: &str) -> String {
        let out = format(text, &Options::default()).unwrap_or_else(|e| panic!("{}: {}", text, e));
        // Formatting is idempotent.
        assert_eq!(
            format(&out, &Options::default()).unwrap(),
            out,
            "not idempotent: {}",
            text
        );
        out
    }

    #[test]
    fn rules_and_blocks() {
        assert_eq!(fmt("{print $1}"), "{\n    print $1\n}\n");
        assert_eq!(
            fmt("BEGIN{FS=\",\"}\nNR>1{s+=$2;n++}\nEND{print s/n}"),
            "BEGIN {\n    FS = \",\"\n}\nNR > 1 {\n    s += $2\n    n++\n}\nEND {\n    print s / n\n}\n"
        );
        assert_eq!(fmt("/foo/"), "/foo/\n");
        assert_eq!(fmt("NR==1,NR==3"), "NR == 1, NR == 3\n");
        assert_eq!(fmt("BEGIN { }"), "BEGIN {}\n");
        assert_eq!(fmt("BEGIN\n{ x = 1 };"), "BEGIN {\n    x = 1\n}\n");
    }

    #[test]
    fn control_flow() {
        assert_eq!(
            fmt("{if(x>1){print \"a\"}\nelse if(x){print \"b\"}else print \"c\"}"),
            "{\n    if (x > 1) {\n        print \"a\"\n    } else if (x) {\n        print \"b\"\n    } else print \"c\"\n}\n"
        );
        assert_eq!(
            fmt("{for(i=1;i<=NF;i++)\nsum+=$i\nprint sum}"),
            "{\n    for (i = 1; i <= NF; i++)\n        sum += $i\n    print sum\n}\n"
        );
        assert_eq!(
            fmt("BEGIN{for(;;)break}"),
            "BEGIN {\n    for (;;) break\n}\n"
        );
        assert_eq!(
            fmt("BEGIN{for(k in a)delete a[k]}"),
            "BEGIN {\n    for (k in a) delete a[k]\n}\n"
        );
        assert_eq!(
            fmt("BEGIN{do{i++}while(i<3);print i}"),
            "BEGIN {\n    do {\n        i++\n    } while (i < 3)\n    print i\n}\n"
        );
        assert_eq!(fmt("BEGIN{while(x--);}"), "BEGIN {\n    while (x--) ;\n}\n");
        assert_eq!(
            fmt(
                "BEGIN{switch(x){case 1:print \"one\";break\ncase /a/: case \"b\":\ndefault: print}}"
            ),
            "BEGIN {\n    switch (x) {\n    case 1:\n        print \"one\"\n        break\n    case /a/:\n    case \"b\":\n    default:\n        print\n    }\n}\n"
        );
    }

    #[test]
    fn functions() {
        assert_eq!(
            fmt("function add(a,b){return a+b}\nBEGIN{print add(1,2)}"),
            "function add(a, b) {\n    return a + b\n}\nBEGIN {\n    print add(1, 2)\n}\n"
        );
        assert_eq!(
            fmt("func f (x)\n{ return -x }"),
            "func f(x) {\n    return -x\n}\n"
        );
    }

    #[test]
    fn operators() {
        assert_eq!(
            fmt("BEGIN{x=-1;y=x- -1;z=2^-1;print !x,-x,x--,--x,$NF,$(NF-1)}"),
            "BEGIN {\n    x = -1\n    y = x - -1\n    z = 2 ^ -1\n    print !x, -x, x--, --x, $NF, $(NF - 1)\n}\n"
        );
        assert_eq!(
            fmt("{a[$1,$2]++;if(($1,$2) in a)print a[$1,$2]>\"out\"}"),
            "{\n    a[$1, $2]++\n    if (($1, $2) in a) print a[$1, $2] > \"out\"\n}\n"
        );
        assert_eq!(
            fmt("{x = a b (c) f\"{x}\" \"s\"}"),
            "{\n    x = a b (c) f\"{x}\" \"s\"\n}\n"
        );
        assert_eq!(
            fmt("{x = a ?b:c ?? d; y = x !~ /re/ && $1 ~ \"x\" || 2**3}"),
            "{\n    x = a ? b : c ?? d\n    y = x !~ /re/ && $1 ~ \"x\" || 2 ** 3\n}\n"
        );
        assert_eq!(
            fmt("{while((\"ls\"|getline line)>0)n++; getline x < \"f\"}"),
            "{\n    while ((\"ls\" | getline line) > 0) n++\n    getline x < \"f\"\n}\n"
        );
        assert_eq!(
            fmt("{print (1,2) > \"f\"; printf(\"%d\\n\", 1); exit(1)}"),
            "{\n    print(1, 2) > \"f\"\n    printf(\"%d\\n\", 1)\n    exit(1)\n}\n"
        );
    }

    #[test]
    fn comments_and_blank_lines() {
        let src = "#!/usr/bin/env -S zawk -E\n# @var n: int\n\n\n\nBEGIN { # start\n  x = 1 # one\n\n  # two\n  y = 2\n}\n# end\n";
        assert_eq!(
            fmt(src),
            "#!/usr/bin/env -S zawk -E\n# @var n: int\n\nBEGIN { # start\n    x = 1 # one\n\n    # two\n    y = 2\n}\n# end\n"
        );
        assert_eq!(
            fmt("BEGIN {\n  # nothing\n}"),
            "BEGIN {\n    # nothing\n}\n"
        );
        assert_eq!(
            fmt("{ if (x) # why\n print }"),
            "{\n    if (x) # why\n        print\n}\n"
        );
    }

    #[test]
    fn line_breaks_in_expressions() {
        assert_eq!(fmt("{print a,\nb}"), "{\n    print a,\n        b\n}\n");
        assert_eq!(
            fmt("$1 > 0 &&\n$2 > 0 { n++ }"),
            "$1 > 0 &&\n    $2 > 0 {\n    n++\n}\n"
        );
        assert_eq!(fmt("{x = 1 + \\\n2}"), "{\n    x = 1 + \\\n        2\n}\n");
        assert_eq!(fmt("{f(a,\nb)}"), "{\n    f(a,\n        b)\n}\n");
    }

    #[test]
    fn tabs() {
        let opts = Options {
            indent: "\t".into(),
        };
        assert_eq!(format("{print}", &opts).unwrap(), "{\n\tprint\n}\n");
    }

    #[test]
    fn syntax_errors() {
        assert!(
            format("BEGIN {", &Options::default())
                .unwrap_err()
                .starts_with("syntax error")
        );
        assert!(format("BEGIN { x = \"a }", &Options::default()).is_err());
    }

    #[test]
    fn empty() {
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("# only a comment"), "# only a comment\n");
    }
}
