use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub syntax);

use crate::arena::Arena;
use crate::ast::{expr_key, uninit_var, Binop, Expr, Pattern, Prog, Stmt};
use crate::builtins::{Function, Variable};
use crate::common::{fatal_message, CompileError, Either, FileSpec, Stage, FATAL_PREFIX, SYNTAX_PREFIX};
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

type ParseResult<'a, T> = Result<T, ParseError<Loc, Tok<'a>, lexer::Error>>;

/// The variable `name` as written at `loc`: the local variable of that name, if one is visible.
pub(crate) fn var<'a>(
    name: &'a str,
    loc: Loc,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> &'a Expr<'a, 'a, &'a str> {
    let e: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::Var(local_name(name, prog)));
    prog.locs.insert(expr_key(e), loc);
    e
}

/// The name of the variable `name`: the new name of the local variable `name`, if one is visible.
pub(crate) fn local_name<'a>(name: &'a str, prog: &Prog<'a, 'a, &'a str>) -> &'a str {
    prog.locals.names.get(name).copied().unwrap_or(name)
}

/// `for (k, v in arr) body`, desugared to `for (k in arr) { v = arr[k]; body }`. `arr` must be an
/// array variable, so that indexing it again in each iteration does not evaluate it again.
pub(crate) fn foreach_kv<'a>(
    k: &'a str,
    v: &'a str,
    arr: &'a Expr<'a, 'a, &'a str>,
    body: &'a Stmt<'a, 'a, &'a str>,
    loc: Loc,
    arena: &'a Arena,
    prog: &Prog<'a, 'a, &'a str>,
) -> ParseResult<'a, &'a Stmt<'a, 'a, &'a str>> {
    if !matches!(arr, Expr::Var(_)) {
        return user_error(loc, "for (k, v in arr) expects an array variable after `in`".into());
    }
    if k == v {
        return user_error(loc, format!("for (k, v in arr) needs two different variables, got `{k}` twice"));
    }
    let (k, v) = (local_name(k, prog), local_name(v, prog));
    let key: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::Var(k));
    let val: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::Var(v));
    let set_v: &'a Stmt<'a, 'a, &'a str> = arena.alloc(Stmt::Expr(arena.alloc(Expr::Assign(val, arena.alloc(Expr::Index(arr, key))))));
    let body = arena.alloc(Stmt::Block(arena.new_vec_from_slice(&[set_v, body])));
    Ok(arena.alloc(Stmt::ForEach(k, arr, body)))
}

/// A declaration of a `local` statement: the name, where it is, and the statement that makes the
/// name a new variable.
pub(crate) type LocalDecl<'a> = (&'a str, Loc, &'a Stmt<'a, 'a, &'a str>);

fn user_error<'a, T>(location: Loc, desc: String) -> ParseResult<'a, T> {
    Err(ParseError::User {
        error: lexer::Error {
            location,
            desc: desc.into(),
        },
    })
}

/// One declaration of a `local` statement, at `loc`: `local x`, `local x = init`, or `local x[]`
/// for an array. `init` is parsed before `x` is declared, so a variable `x` in it is the outer one
/// (`local x = x + 1`). Returns the statement that makes `x` a new variable (see `LocalScope`).
pub(crate) fn declare_local<'a>(
    name: &'a str,
    init: Option<&'a Expr<'a, 'a, &'a str>>,
    array: bool,
    loc: Loc,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> ParseResult<'a, LocalDecl<'a>> {
    // SUBSEP is an ordinary variable for the rest of the compiler, but `a[i, j]` reads the global.
    if Variable::try_from(name).is_ok() || name == "length" || name == "SUBSEP" {
        return user_error(loc, format!("cannot declare the special variable `{}' as local", name));
    }
    prog.locals.count += 1;
    let new: &'a str = arena.alloc_str(&format!("{}@{}", name, prog.locals.count));
    let v: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::Var(new));
    prog.locs.insert(expr_key(v), loc);
    let stmt = match (init, array) {
        (Some(init), _) => Expr::Assign(v, init),
        (None, true) => Expr::Call(Either::Right(Function::Clear), arena.alloc_slice(&[v])),
        (None, false) => Expr::Assign(v, arena.alloc(Expr::Var(arena.alloc_str(&uninit_var(new))))),
    };
    let stmt: &'a Stmt<'a, 'a, &'a str> = arena.alloc(Stmt::Expr(arena.alloc(stmt)));
    prog.locals.names.insert(name, new);
    prog.locals.decls.push((new, array, expr_key(stmt)));
    prog.locals.declared.push((name, loc));
    Ok((name, loc, stmt))
}

/// The statement that makes the local variable `name` uninitialized, or empties the local array.
fn reset_local<'a>(name: &'a str, array: bool, arena: &'a Arena) -> &'a Stmt<'a, 'a, &'a str> {
    let v: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::Var(name));
    let e = if array {
        Expr::Call(Either::Right(Function::Clear), arena.alloc_slice(&[v]))
    } else {
        Expr::Assign(v, arena.alloc(Expr::Var(arena.alloc_str(&uninit_var(name)))))
    };
    arena.alloc(Stmt::Expr(arena.alloc(e)))
}

/// The `local` statement made of the declarations `decls`, which must declare different names.
/// An expression statement. `assert(cond, msg)` is desugared here to
/// `if (cond) ; else { print msg > "/dev/stderr"; exit 2 }`, so that `cond` is tested as by `if`.
pub(crate) fn expr_stmt<'a>(
    e: &'a Expr<'a, 'a, &'a str>,
    loc: Loc,
    arena: &'a Arena,
) -> ParseResult<'a, &'a Stmt<'a, 'a, &'a str>> {
    let &Expr::Call(Either::Left("assert"), args) = e else {
        return Ok(arena.alloc(Stmt::Expr(e)));
    };
    let &[cond, msg] = args else {
        return user_error(loc, format!("assert expects 2 arguments (condition, message), got {}", args.len()));
    };
    let stderr: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::StrLit(b"/dev/stderr"));
    let print: &'a Stmt<'a, 'a, &'a str> = arena.alloc(Stmt::Print(arena.alloc_slice(&[msg]), Some((stderr, FileSpec::Trunc))));
    let code: &'a Expr<'a, 'a, &'a str> = arena.alloc(Expr::ILit(2));
    let exit: &'a Stmt<'a, 'a, &'a str> =
        arena.alloc(Stmt::Expr(arena.alloc(Expr::Call(Either::Right(Function::Exit), arena.alloc_slice(&[code])))));
    let fail = arena.alloc(Stmt::Block(arena.new_vec_from_slice(&[print, exit])));
    Ok(arena.alloc(Stmt::If(cond, arena.alloc(Stmt::Block(arena.new_vec())), Some(fail))))
}

pub(crate) fn local_stmt<'a>(
    decls: Vec<LocalDecl<'a>>,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> ParseResult<'a, &'a Stmt<'a, 'a, &'a str>> {
    for (i, (name, loc, _)) in decls.iter().enumerate() {
        if decls[..i].iter().any(|(prev, _, _)| prev == name) {
            return user_error(*loc, format!("local variable `{}' is declared twice in the same statement", name));
        }
    }
    if decls.len() == 1 {
        return Ok(decls[0].2);
    }
    let stmts: Vec<_> = decls.iter().map(|d| d.2).collect();
    let stmt: &'a Stmt<'a, 'a, &'a str> = arena.alloc(Stmt::Block(arena.new_vec_from_slice(&stmts[..])));
    let n = prog.locals.decls.len();
    for d in prog.locals.decls[n - decls.len()..].iter_mut() {
        d.2 = expr_key(stmt);
    }
    Ok(stmt)
}

/// Rejects a local variable named after a function, which may be defined after it, as gawk
/// rejects such a variable.
pub(crate) fn check_local_names<'a>(prog: &Prog<'a, 'a, &'a str>) -> ParseResult<'a, ()> {
    for (name, loc) in prog.locals.declared.iter() {
        if prog.decs.iter().any(|d| d.name == *name) {
            return user_error(*loc, format!("function `{}' cannot be declared as a local variable", name));
        }
    }
    Ok(())
}

/// Ends the scope of the local variables of the rule whose action is `body`. A local declared in a
/// nested statement (`if (c) { local x = 1 }`) may be read without running its declaration, so
/// the rule starts by making it uninitialized.
pub(crate) fn end_rule_scope<'a>(
    body: &'a Stmt<'a, 'a, &'a str>,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> &'a Stmt<'a, 'a, &'a str> {
    let decls = std::mem::take(&mut prog.locals.decls);
    prog.locals.names.clear();
    let top: Vec<usize> = match body {
        Stmt::Block(stmts) => stmts.iter().map(|s| expr_key(*s)).collect(),
        s => vec![expr_key(s)],
    };
    let mut stmts: Vec<_> = decls
        .iter()
        .filter(|(_, _, key)| !top.contains(key))
        .map(|(name, array, _)| reset_local(name, *array, arena))
        .collect();
    if stmts.is_empty() {
        return body;
    }
    stmts.push(body);
    arena.alloc(Stmt::Block(arena.new_vec_from_slice(&stmts[..])))
}

/// Ends the scope of the local variables of a function, and returns its parameters followed by
/// its locals. A parameter that is not passed is uninitialized on each call, so the locals are
/// too; the `uninit_var` of a scalar local is one as well.
pub(crate) fn end_function_scope<'a>(
    mut args: Vec<&'a str>,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> Vec<&'a str> {
    let decls = std::mem::take(&mut prog.locals.decls);
    prog.locals.names.clear();
    for (name, array, _) in decls.iter() {
        if !array {
            args.push(arena.alloc_str(&uninit_var(name)));
        }
    }
    args.extend(decls.iter().map(|(name, _, _)| *name));
    args
}

type FStrResult<'a> = Result<&'a Expr<'a, 'a, &'a str>, ParseError<Loc, Tok<'a>, lexer::Error>>;

/// Desugars the f-string literal `f"...{expr}...{expr:spec}..."` (`lit` is the text between the
/// quotes, starting at `loc`) into a concatenation of string literals and expressions.
///
/// A placeholder holds any expression (`{NR+1}`, `{$1}`, `{toupper(name)}`, `{a[\"k\"]}`: a `"`
/// is written `\"`, as the f-string ends at a bare `"`), optionally followed by a format spec
/// after a `:` that is not part of a `?:` (see `format_spec`). `{{` and `}}` are literal braces,
/// and an empty placeholder `{}` is kept as text. A `{` without a matching `}` is an error: it
/// usually comes from a bare `"` inside a placeholder, which ends the f-string.
pub(crate) fn parse_fstring_literal<'a>(
    lit: &'a str,
    loc: Loc,
    arena: &'a Arena,
    buf: &mut Vec<u8>,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> FStrResult<'a> {
    let error = |at: usize, desc: String| ParseError::User {
        error: lexer::Error {
            location: loc.advance("f\"").advance(&lit[..at]),
            desc: desc.into(),
        },
    };
    let mut parts: Vec<&'a Expr<'a, 'a, &'a str>> = Vec::new();
    // Raw literal text: escapes are processed by `parse_string_literal`.
    let mut text = String::new();
    let flush = |text: &mut String, parts: &mut Vec<_>, buf: &mut Vec<u8>| {
        if !text.is_empty() {
            let s = lexer::parse_string_literal(text, arena, buf);
            parts.push(&*arena.alloc(Expr::StrLit(s)));
            text.clear();
        }
    };
    let bytes = lit.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                let n = lit[i + 1..].chars().next().map_or(0, char::len_utf8);
                text.push_str(&lit[i..i + 1 + n]);
                i += 1 + n;
            }
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                text.push('{');
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                text.push('}');
                i += 2;
            }
            b'{' => {
                let start = i + 1;
                let Some((colon, end)) = placeholder_end(&lit[start..]) else {
                    return Err(error(
                        i,
                        "unclosed `{` in f-string: write `{{` for a literal `{`, and use \\\" for quotes inside a placeholder, as in f\"{a[\\\"k\\\"]}\"".into(),
                    ));
                };
                let expr_end = start + colon.unwrap_or(end);
                let src = &lit[start..expr_end];
                if src.trim().is_empty() {
                    if colon.is_some() {
                        return Err(error(start, "empty expression in f-string placeholder".into()));
                    }
                    text.push_str(&lit[i..start + end + 1]);
                    i = start + end + 1;
                    continue;
                }
                flush(&mut text, &mut parts, buf);
                let e = parse_placeholder(src, loc.advance("f\"").advance(&lit[..start]), arena, prog)?;
                let e = match colon {
                    Some(c) => format_spec(e, &lit[start + c + 1..start + end], arena)
                        .map_err(|desc| error(start + c + 1, desc))?,
                    None => e,
                };
                parts.push(e);
                i = start + end + 1;
            }
            _ => {
                let n = lit[i..].chars().next().map_or(1, char::len_utf8);
                text.push_str(&lit[i..i + n]);
                i += n;
            }
        }
    }
    flush(&mut text, &mut parts, buf);
    let mut parts = parts.into_iter();
    // Start from a string literal so that the result is always a string, as in `f"{x}"`.
    let mut res: &'a Expr<'a, 'a, &'a str> = match parts.next() {
        Some(e @ Expr::StrLit(_)) => e,
        Some(e) => arena.alloc(Expr::Binop(Binop::Concat, arena.alloc(Expr::StrLit(b"")), e)),
        None => arena.alloc(Expr::StrLit(b"")),
    };
    for e in parts {
        res = arena.alloc(Expr::Binop(Binop::Concat, res, e));
    }
    Ok(res)
}

/// Finds the end of the placeholder `s` (the text after its `{`): the offset of the `:` starting a
/// format spec, if any, and of the closing `}`. Brackets nest, `\"...\"` strings are skipped, and a
/// `:` closing a `?` belongs to the expression.
fn placeholder_end(s: &str) -> Option<(Option<usize>, usize)> {
    let bytes = s.as_bytes();
    let (mut depth, mut ternary, mut in_str, mut colon) = (0usize, 0usize, false, None);
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if bytes.get(i + 1) == Some(&b'"') {
                    in_str = !in_str;
                }
                i += 2;
                continue;
            }
            _ if in_str => {}
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' if depth > 0 => depth -= 1,
            b'}' if depth > 0 => depth -= 1,
            b'}' => return Some((colon, i)),
            _ if depth > 0 || colon.is_some() => {}
            b'?' => ternary += 1,
            b':' if ternary > 0 => ternary -= 1,
            b':' => colon = Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Parses the expression of an f-string placeholder, which starts at `loc` in the program.
fn parse_placeholder<'a>(
    src: &str,
    loc: Loc,
    arena: &'a Arena,
    prog: &mut Prog<'a, 'a, &'a str>,
) -> FStrResult<'a> {
    // `\"` is a quote in the f-string; parse the expression as the pattern `(expr)`.
    let src = src.replace("\\\"", "\"");
    let text: &'a str = arena.alloc_str(&format!("({})\n", src));
    // Maps a location in `text` back to the program, ignoring the `\` dropped before quotes.
    let map = |l: Loc| {
        let k = l.offset().saturating_sub(1).min(src.len());
        loc.advance(src.get(..k).unwrap_or(&src))
    };
    let mut inner = Prog::from_stage(arena, Stage::Main(()));
    inner.locals.names = prog.locals.names.clone();
    let parsed = syntax::ProgParser::new().parse(arena, &mut Vec::new(), &mut inner, lexer::Tokenizer::new(text));
    let e = match (parsed, inner.pats.first()) {
        (Ok(()), Some((Pattern::Bool(e), None))) if inner.pats.len() == 1 => *e,
        (Ok(()), _) => {
            return Err(ParseError::User {
                error: lexer::Error {
                    location: loc,
                    desc: format!("invalid expression in f-string placeholder `{{{}}}`", src.trim()).into(),
                },
            })
        }
        (Err(err), _) => {
            let (l, mut desc) = describe_parse_error(text, &err);
            // An error at the closing `)` we added: the expression ends too early.
            if l.offset() > src.len() {
                desc = "incomplete expression".into();
            }
            return Err(ParseError::User {
                error: lexer::Error {
                    location: map(l),
                    desc: format!("in f-string placeholder `{{{}}}`: {}", src.trim(), desc).into(),
                },
            });
        }
    };
    prog.locs.extend(inner.locs.into_iter().map(|(k, l)| (k, map(l))));
    Ok(e)
}

/// Applies the format spec of an f-string placeholder (as in Python, without `=` alignment):
/// `[[fill]align][sign][#][0][width][grouping][.precision][type]`, where `align` is `<`, `>` or
/// `^`, `sign` is `+`, `-` or a space, `grouping` is `,` or `_` (the thousands separator, printf's
/// `'` and `_` flags), and `type` is a printf conversion (`d i o x X e E f F g G s c`) or `%` (the
/// number times 100, in `f` format, with a `%` sign). Without a type the value is formatted as a
/// string (`%s`, so `.n` truncates; `0` pads it with zeros on the left; with a grouping, a value
/// that looks like a number has its digits grouped). Values are right-aligned by default.
fn format_spec<'a>(
    e: &'a Expr<'a, 'a, &'a str>,
    spec: &str,
    arena: &'a Arena,
) -> Result<&'a Expr<'a, 'a, &'a str>, String> {
    let invalid = || format!("invalid format spec `{}` in f-string placeholder", spec);
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let is_align = |c: Option<&char>| matches!(c, Some('<' | '>' | '^'));
    let (fill, align) = if is_align(chars.get(1)) {
        i = 2;
        (Some(chars[0]), Some(chars[1]))
    } else if is_align(chars.first()) {
        i = 1;
        (None, Some(chars[0]))
    } else {
        (None, None)
    };
    let mut flags = String::new();
    if let Some(&c @ ('+' | '-' | ' ')) = chars.get(i) {
        if c != '-' {
            flags.push(c);
        }
        i += 1;
    }
    if chars.get(i) == Some(&'#') {
        flags.push('#');
        i += 1;
    }
    let mut zero = false;
    if chars.get(i) == Some(&'0') {
        zero = true;
        i += 1;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while chars.get(*i).is_some_and(char::is_ascii_digit) {
            *i += 1;
        }
        chars[start..*i].iter().collect::<String>()
    };
    let width = digits(&mut i);
    let group = match chars.get(i) {
        Some(',') => Some('\''),
        Some('_') => Some('_'),
        _ => None,
    };
    if group.is_some() {
        i += 1;
    }
    let mut precision = None;
    if chars.get(i) == Some(&'.') {
        i += 1;
        let p = digits(&mut i);
        if p.is_empty() {
            return Err(invalid());
        }
        precision = Some(p);
    }
    let typed = i < chars.len();
    let ty = match chars.get(i) {
        None => 's',
        Some(&c) if "dioxXeEfFgGsc%".contains(c) => {
            i += 1;
            c
        }
        Some(_) => return Err(invalid()),
    };
    if i != chars.len() || (group.is_some() && "oxXcs".contains(ty) && typed) {
        return Err(invalid());
    }
    flags.extend(group);
    let (conv, suffix, arg) = match ty {
        'i' => ('d', "", e),
        '%' => ('f', "%%", &*arena.alloc(Expr::Binop(Binop::Mult, e, arena.alloc(Expr::ILit(100))))),
        c => (c, "", e),
    };
    let precision = precision.map_or(String::new(), |p| format!(".{}", p));
    let sprintf = |fmt: String| -> &'a Expr<'a, 'a, &'a str> {
        let fmt = arena.alloc(Expr::StrLit(arena.alloc_bytes(fmt.as_bytes())));
        arena.alloc(Expr::Call(Either::Left("sprintf"), arena.alloc_slice(&[&*fmt, arg])))
    };
    // printf pads with spaces (or zeros); other fills, centering and zero-padding a value of
    // unknown type (`%05s` pads with spaces) use the pad functions.
    let (fill, pad_fn) = match (align, fill) {
        (None, None) if zero && !typed && group.is_none() => (Some('0'), Some("pad_start")),
        (Some('^'), _) => (fill, Some("pad")),
        (Some('<'), Some(f)) if f != ' ' => (fill, Some("pad_end")),
        (Some('>'), Some(f)) if f != ' ' => (fill, Some("pad_start")),
        _ => (fill, None),
    };
    if let (Some(pad_fn), false) = (pad_fn, width.is_empty()) {
        let s = sprintf(format!("%{}{}{}{}", flags, precision, conv, suffix));
        let width = arena.alloc(Expr::ILit(width.parse().map_err(|_| invalid())?));
        let fill = fill.unwrap_or(' ').to_string();
        let fill = arena.alloc(Expr::StrLit(arena.alloc_bytes(fill.as_bytes())));
        return Ok(arena.alloc(Expr::Call(Either::Left(pad_fn), arena.alloc_slice(&[s, &*width, &*fill]))));
    }
    if align == Some('<') {
        flags.push('-');
    } else if zero {
        flags.push('0');
    }
    Ok(sprintf(format!("%{}{}{}{}{}", flags, width, precision, conv, suffix)))
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
        } => match unsupported_field_operand(prog, l, r) {
            Some(msg) => (*l, msg),
            None => (
                *l,
                format!("unexpected {}{}", token_text(prog, l, r), expected_tokens(expected)),
            ),
        },
        ParseError::ExtraToken { token: (l, _, r) } => {
            (*l, format!("unexpected {}", token_text(prog, l, r)))
        }
        ParseError::User { error } => (error.location, error.desc.to_string()),
    }
}

/// `$` followed by another `$` or a unary operator (`$$1`, `$++i`, `$-1`, `$!x`) is not supported:
/// the operand of `$` must be a number, a variable, a call or a parenthesized expression. Returns the
/// message for the token at `l..r` when it is such an operator right after a `$`.
pub fn unsupported_field_operand(prog: &str, l: &Loc, r: &Loc) -> Option<String> {
    let op = prog.get(l.offset()..r.offset())?.trim();
    if !matches!(op, "$" | "++" | "--" | "-" | "+" | "!") {
        return None;
    }
    if !prog.get(..l.offset())?.trim_end_matches([' ', '\t']).ends_with('$') {
        return None;
    }
    let operand = match op {
        "$" => "$1".to_string(),
        "++" | "--" => format!("{}i", op),
        _ => format!("{}1", op),
    };
    Some(format!(
        "unsupported syntax: `{}` right after `$`; parenthesize the field number, as in `$({})`",
        op, operand
    ))
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
    if let Some(msg) = err.0.strip_prefix(SYNTAX_PREFIX) {
        return match (err.1, PROGRAM.get()) {
            (Some(loc), Some((prog, sources))) => format!(
                "zawk: syntax error at {}: {}{}",
                sources.describe(prog, loc),
                msg,
                excerpt(prog, loc)
            ),
            _ => format!("zawk: syntax error: {}", msg),
        };
    }
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
