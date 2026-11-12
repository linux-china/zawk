//! Safe automatic parallelism: whether a program run without `-p` prints the same result with
//! `-pr`, so that it may run in parallel on large inputs without being asked to.
//!
//! The check is deliberately conservative: it accepts the aggregation shape that `-pr` handles
//! exactly (see `info/parallelism.md`) and refuses everything else.
//!
//! * The main loop has no visible effect: no `print`/`printf`, `getline`, `system`, `exit`,
//!   network or file functions, user-defined functions, range patterns or rules without an
//!   action (whose action is `print`); it calls only pure builtins (`PURE_BUILTINS`).
//! * Every global the main loop writes is either
//!   - **summed**: only updated by statements `x += e`, `x -= e`, `x++`, `x--` (or the same on
//!     elements `a[k]`) where `e` is a small whole number (`int_valued`: a count, a truth value,
//!     a small literal), never read in the main loop and not mentioned in BEGIN, in a function
//!     or by `-v`. END sees it summed over the workers, which is exactly what the serial run
//!     computed. A sum of fractions such as `s += $3` is not: the workers add in another order,
//!     which changes the last bits of the result (enough to print `2.9997e+06` instead of
//!     `2999700`) and from one run to the next; nor is a sum of `int($3)`, whose partial sums
//!     may pass 2^53 (epoch milliseconds, byte counts), where f64 additions of whole numbers
//!     round too; both take an explicit `-pr`; or
//!   - **per record**: mentioned in a single action only, whose first top-level statement that
//!     mentions it assigns it without reading it (`x = e`, `n = split(s, parts)`,
//!     `for (i = 1; ...)`, `for (k in a)`); it is not mentioned in BEGIN, END, functions or `-v`.
//!
//!   Globals the main loop only reads (set in BEGIN or with `-v`) are copied to the workers.
//! * The main loop does not read `NR`, `FNR`, `FILENAME` or `PID`, which count per worker, nor
//!   assign a special variable.
//! * END does not read the last record (`$0`, `$n`, `NF`) or `RSTART`/`RLENGTH`.
//! * END does not walk an array the main loop fills with `for (k in a)` unless BEGIN or END
//!   sets `PROCINFO["sorted_in"]`: the keys of an array merged from the workers come in another
//!   order than in a serial run, so the output would change once the input grows large enough.
//! * No plain `getline` anywhere (it reads the main input) and no assignment to `RS` anywhere
//!   (a record separator assigned at run time needs the serial regex splitter).
//! * No `PREPARE`, `BEGINFILE` or `ENDFILE` rules.
use std::collections::{HashMap, HashSet};

use crate::arena::Arena;
use crate::ast::{self, Binop, Expr, Pattern, Stmt, Unop};
use crate::builtins::Function;
use crate::common::{Either, Stage};
use crate::{lexer, parsing};

type E<'a> = Expr<'a, 'a, &'a str>;
type S<'a> = Stmt<'a, 'a, &'a str>;

/// Builtins without side effects whose result does not depend on which worker calls them.
const PURE_BUILTINS: &[&str] = &[
    "length",
    "substr",
    "index",
    "split",
    "sub",
    "gsub",
    "gensub",
    "match",
    "sprintf",
    "tolower",
    "toupper",
    "int",
    "sin",
    "cos",
    "atan2",
    "exp",
    "log",
    "sqrt",
    "strtonum",
    "and",
    "or",
    "xor",
    "compl",
    "lshift",
    "rshift",
    "rshiftl",
    "typeof",
    "isarray",
    "isint",
    "isnum",
    "isnull",
    "abs",
    "ceil",
    "floor",
    "round",
    "log2",
    "log10",
    "intdiv0",
    "min",
    "max",
    "trim",
    "truncate",
    "starts_with",
    "ends_with",
    "contains",
    "eq_ignore_case",
    "capitalize",
    "uncapitalize",
    "camel_case",
    "kebab_case",
    "snake_case",
    "title_case",
    "pascal_case",
    "pad",
    "pad_start",
    "pad_end",
    "strcmp",
    "repeat",
    "default_if_empty",
    "append_if_missing",
    "prepend_if_missing",
    "remove_if_end",
    "remove_if_begin",
    "quote",
    "double_quote",
    "char_at",
    "last_index",
    "last_part",
    "float",
    "hex",
    "format_bytes",
    "to_bytes",
    "escape_csv",
    "escape_tsv",
    "mask",
];

/// Variables whose value in the main loop differs between a worker and the serial run.
const PER_WORKER_VARIABLES: &[&str] = &["NR", "FNR", "FILENAME", "PID"];

/// Whether `text` (the whole program) gives the same result run with `-pr` as serially.
/// `given` are the variables set with `-v`. A program that does not parse is not parallel.
pub fn record_parallel_safe(text: &str, given: &[&str]) -> bool {
    let a = Arena::default();
    let text: &str = a.alloc_str(text);
    let mut buf = Vec::new();
    let mut prog = ast::Prog::from_stage(&a, Stage::Main(()));
    let parser = parsing::syntax::ProgParser::new();
    if parser
        .parse(&a, &mut buf, &mut prog, lexer::Tokenizer::new(text))
        .is_err()
    {
        return false;
    }
    check(&prog, given)
}

fn check<'a>(prog: &ast::Prog<'a, 'a, &'a str>, given: &[&str]) -> bool {
    if prog.pats.is_empty()
        || !prog.prepare.is_empty()
        || !prog.beginfile.is_empty()
        || !prog.endfile.is_empty()
    {
        return false;
    }
    // Everything outside the main loop: BEGIN, functions and `-v`.
    let mut outside = Names::default();
    outside.all.extend(given.iter().copied());
    for s in prog.begin.iter() {
        outside.stmt(s);
    }
    for d in prog.decs.iter() {
        outside.all.extend(d.args.iter().copied());
        outside.stmt(d.body);
    }
    let mut end = Names::default();
    for s in prog.end.iter() {
        end.stmt(s);
    }
    if outside.bad || end.bad || end.reads_record {
        return false;
    }
    if ["RSTART", "RLENGTH"].iter().any(|v| end.all.contains(v)) {
        return false;
    }
    let user_fns: HashSet<&str> = prog.decs.iter().map(|d| d.name).collect();

    // The main loop: what each action mentions, and how.
    let mut main = Names::default();
    let mut summed: HashSet<&'a str> = HashSet::new();
    // Summed variables to which a value that may be a fraction is added.
    let mut fraction_summed: HashSet<&'a str> = HashSet::new();
    // Name -> the actions (by index) that mention it other than in a sum, `usize::MAX` for a
    // pattern.
    let mut mentioned: HashMap<&'a str, HashSet<usize>> = HashMap::new();
    let mut bodies: Vec<Vec<&'a S<'a>>> = Vec::new();
    let actions: Vec<&'a S<'a>> = prog.pats.iter().filter_map(|(_, b)| *b).collect();
    let ints = int_vars(&actions);
    for (i, (pat, body)) in prog.pats.iter().enumerate() {
        let pat_expr = match pat {
            Pattern::Null => None,
            Pattern::Bool(e) => Some(*e),
            Pattern::Comma(..) => return false,
        };
        // A rule without an action prints its records.
        let Some(body) = body else { return false };
        let mut v = MainVisitor {
            user_fns: &user_fns,
            ok: true,
            summed: &mut summed,
            fraction_summed: &mut fraction_summed,
            ints: &ints,
            mentioned: &mut mentioned,
            action: usize::MAX,
        };
        if let Some(e) = pat_expr {
            v.expr(e);
            main.expr(e);
        }
        v.action = i;
        v.stmt(body);
        if !v.ok {
            return false;
        }
        main.stmt(body);
        bodies.push(top_level(body));
    }
    if main.bad {
        return false;
    }
    let is_special = |n: &str| crate::builtins::VARIABLES.contains_key(n);
    if main.written.iter().any(|n| is_special(n)) || summed.iter().any(|n| is_special(n)) {
        return false;
    }
    // `for (k in a)` in END over an array filled by the workers visits its keys in another order
    // than a serial run, unless the program asks for an order.
    let mut walked = HashSet::new();
    prog.end.iter().for_each(|s| for_in_arrays(s, &mut walked));
    if walked
        .iter()
        .any(|n| summed.contains(n) || main.written.contains(n))
        && !prog
            .begin
            .iter()
            .chain(prog.end.iter())
            .any(|s| sets_sorted_in(s))
    {
        return false;
    }
    let local = |n: &str| n.contains('@');
    for name in summed.iter() {
        if local(name) || mentioned.contains_key(name) {
            continue; // checked as a per-record variable below
        }
        if outside.all.contains(name) || fraction_summed.contains(name) {
            return false;
        }
    }
    for (name, actions) in mentioned.iter() {
        if local(name) {
            continue;
        }
        if PER_WORKER_VARIABLES.contains(name) {
            return false;
        }
        if !main.written.contains(name) && !summed.contains(name) {
            continue; // read only
        }
        // Written in the main loop and read there: it must be set anew for each record.
        if outside.all.contains(name) || end.all.contains(name) || actions.len() != 1 {
            return false;
        }
        let i = *actions.iter().next().unwrap();
        if i == usize::MAX {
            return false;
        }
        match bodies[i].iter().find(|s| stmt_mentions(s, name)) {
            Some(s) if defines_first(s, name) => {}
            _ => return false,
        }
    }
    true
}

/// The bound on the magnitude of a count (`length`, `split`, `index`, ...), of a variable of
/// `ints` and of an integer literal that may be summed.
const COUNT_BOUND: f64 = (1u64 << 31) as f64;
/// The largest magnitude of a whole number that `int_valued` accepts per record. Adding whole
/// numbers in f64 does not depend on the order of the additions only while the partial sums
/// stay below 2^53: this keeps them there for billions of records.
const SUMMAND_BOUND: f64 = (1u64 << 32) as f64;

/// Whether `e` is always a whole number of a small magnitude (at most `SUMMAND_BOUND`), so that
/// sums of it do not depend on the order of the additions: small integer literals, counts
/// (`length`, `split`, `index`, `match`, ...), truth values (comparisons, `!`, `&&`, `||`,
/// `in`), the variables `ints` and `+`, `-`, `*`, `%` of these. Fields and other variables may
/// hold fractions; `int()` and bitwise functions may hold whole numbers beyond 2^53, whose sums
/// do depend on the order (`s += int($1)` over epoch milliseconds).
fn int_valued(e: &E, ints: &HashSet<&str>) -> bool {
    int_bound(e, ints).is_some_and(|b| b <= SUMMAND_BOUND)
}

/// A bound on the magnitude of `e` if it is always a whole number (infinite when the magnitude
/// is unknown), `None` if it may hold a fraction or a string.
fn int_bound(e: &E, ints: &HashSet<&str>) -> Option<f64> {
    let bound = |e: &E| int_bound(e, ints);
    match e {
        Expr::Var(n) => ints.contains(n).then_some(COUNT_BOUND),
        Expr::ILit(i) => Some((*i as f64).abs()),
        Expr::FLit(f) => (f.fract() == 0.0).then_some(f.abs()),
        Expr::Unop(Unop::Not, _) => Some(1.0),
        Expr::Unop(Unop::Neg | Unop::Pos, x) => bound(x),
        Expr::Binop(Binop::Plus | Binop::Minus, l, r) => Some(bound(l)? + bound(r)?),
        Expr::Binop(Binop::Mult, l, r) => Some(bound(l)? * bound(r)?),
        // |l % r| < |r| and <= |l|.
        Expr::Binop(Binop::Mod, l, r) => Some(bound(l)?.min(bound(r)?)),
        Expr::Binop(
            Binop::LT | Binop::GT | Binop::LTE | Binop::GTE | Binop::EQ | Binop::IsMatch,
            _,
            _,
        ) => Some(1.0),
        Expr::And(..) | Expr::Or(..) => Some(1.0),
        Expr::ITE(_, t, f) => Some(bound(t)?.max(bound(f)?)),
        Expr::Call(Either::Right(f), _) => match f {
            Function::Contains => Some(1.0),
            Function::Length => Some(COUNT_BOUND),
            _ => None,
        },
        Expr::Call(Either::Left(name), _) => match *name {
            "isint" | "isnum" | "isnull" | "isarray" | "strcmp" | "starts_with" | "ends_with"
            | "contains" | "eq_ignore_case" => Some(1.0),
            "length" | "split" | "index" | "match" | "sub" | "gsub" | "last_index" => {
                Some(COUNT_BOUND)
            }
            // Whole numbers of any magnitude.
            "int" | "and" | "or" | "xor" | "compl" | "lshift" | "rshift" | "rshiftl" => {
                Some(f64::INFINITY)
            }
            _ => None,
        },
        _ => None,
    }
}

/// The variables that the main loop `bodies` only assign whole numbers (`n = split(...)`,
/// `i++`, `for (i in 1..n)`): the others may hold fractions or strings.
fn int_vars<'a>(bodies: &[&'a S<'a>]) -> HashSet<&'a str> {
    #[derive(Default)]
    struct Writes<'a> {
        /// Name -> the values assigned to it; `None` for a value that is not an expression.
        values: HashMap<&'a str, Vec<Option<&'a E<'a>>>>,
    }
    impl<'a> Writes<'a> {
        fn expr(&mut self, e: &'a E<'a>) {
            let mut names = Names::default();
            names.expr(e);
            // Every write, then the values of plain assignments and increments.
            for n in names.written {
                self.values.entry(n).or_default();
            }
            self.find(e);
        }
        fn find(&mut self, e: &'a E<'a>) {
            match e {
                Expr::Assign(Expr::Var(n), r) => {
                    self.values.entry(n).or_default().push(Some(*r));
                    self.find(r);
                }
                Expr::AssignOp(Expr::Var(n), op, r) => {
                    let ok = matches!(op, Binop::Plus | Binop::Minus | Binop::Mult | Binop::Mod);
                    self.values
                        .entry(n)
                        .or_default()
                        .push(if ok { Some(*r) } else { None });
                    self.find(r);
                }
                Expr::Inc {
                    x: Expr::Var(_), ..
                } => {}
                Expr::Unop(_, x) => self.find(x),
                Expr::Binop(_, l, r)
                | Expr::Index(l, r)
                | Expr::And(l, r)
                | Expr::Or(l, r)
                | Expr::Nullish(l, r)
                | Expr::Try(l, r)
                | Expr::Assign(l, r)
                | Expr::AssignOp(l, _, r) => {
                    self.find(l);
                    self.find(r);
                }
                Expr::Inc { x, .. } => self.find(x),
                Expr::ITE(c, t, f) => {
                    self.find(c);
                    self.find(t);
                    self.find(f);
                }
                Expr::Call(Either::Left("sub" | "gsub"), args) => {
                    // The target becomes a string.
                    if let Some(Expr::Var(n)) = args.get(2) {
                        self.values.entry(n).or_default().push(None);
                    }
                    args.iter().for_each(|a| self.find(a));
                }
                Expr::Call(_, args) => args.iter().for_each(|a| self.find(a)),
                Expr::Getline {
                    into: Some(Expr::Var(n)),
                    ..
                } => self.values.entry(n).or_default().push(None),
                _ => {}
            }
        }
        fn stmt(&mut self, s: &'a S<'a>) {
            match s {
                Stmt::Expr(e) => self.expr(e),
                Stmt::Block(v) => v.iter().for_each(|s| self.stmt(s)),
                Stmt::If(c, t, f) => {
                    self.expr(c);
                    self.stmt(t);
                    if let Some(f) = f {
                        self.stmt(f);
                    }
                }
                Stmt::For(init, cond, update, body) => {
                    init.iter().for_each(|s| self.stmt(s));
                    cond.iter().for_each(|e| self.expr(e));
                    update.iter().for_each(|s| self.stmt(s));
                    self.stmt(body);
                }
                Stmt::DoWhile(c, b) | Stmt::While(_, c, b) => {
                    self.expr(c);
                    self.stmt(b);
                }
                Stmt::ForEach(v, arr, body) => {
                    // A key, which may be any string.
                    self.values.entry(v).or_default().push(None);
                    self.expr(arr);
                    self.stmt(body);
                }
                Stmt::ForRange(v, from, to, _, body) => {
                    self.values.entry(v).or_default();
                    self.expr(from);
                    self.expr(to);
                    self.stmt(body);
                }
                Stmt::Switch(e, cases) => {
                    self.expr(e);
                    cases.iter().for_each(|(_, b)| self.stmt(b));
                }
                _ => {}
            }
        }
    }
    let mut w = Writes::default();
    bodies.iter().for_each(|s| w.stmt(s));
    // Assume all of them hold whole numbers, and drop those assigned something else until
    // nothing changes.
    let mut ints: HashSet<&'a str> = w.values.keys().copied().collect();
    loop {
        let drop: Vec<&str> = ints
            .iter()
            .filter(|n| {
                w.values[*n]
                    .iter()
                    .any(|v| !v.is_some_and(|e| int_valued(e, &ints)))
            })
            .copied()
            .collect();
        if drop.is_empty() {
            return ints;
        }
        drop.iter().for_each(|n| {
            ints.remove(n);
        });
    }
}

/// The arrays that `for (k in a)` statements in `s` walk.
fn for_in_arrays<'a>(s: &'a S<'a>, out: &mut HashSet<&'a str>) {
    match s {
        Stmt::ForEach(_, arr, body) => {
            if let Expr::Var(n) = arr {
                out.insert(n);
            }
            for_in_arrays(body, out);
        }
        Stmt::Block(v) => v.iter().for_each(|s| for_in_arrays(s, out)),
        Stmt::If(_, t, f) => {
            for_in_arrays(t, out);
            if let Some(f) = f {
                for_in_arrays(f, out);
            }
        }
        Stmt::For(_, _, _, b)
        | Stmt::DoWhile(_, b)
        | Stmt::While(_, _, b)
        | Stmt::ForRange(_, _, _, _, b)
        | Stmt::EndBlock(b) => for_in_arrays(b, out),
        Stmt::Switch(_, cases) => cases.iter().for_each(|(_, b)| for_in_arrays(b, out)),
        _ => {}
    }
}

/// Whether `s` contains `PROCINFO["sorted_in"] = "@..."`, which fixes the order of
/// `for (k in a)`.
fn sets_sorted_in(s: &S) -> bool {
    fn expr(e: &E) -> bool {
        match e {
            Expr::Assign(Expr::Index(Expr::Var("PROCINFO"), Expr::StrLit(b"sorted_in")), v) => {
                matches!(v, Expr::StrLit(order) if order.starts_with(b"@"))
            }
            _ => false,
        }
    }
    match s {
        Stmt::Expr(e) => expr(e),
        Stmt::Block(v) => v.iter().any(|s| sets_sorted_in(s)),
        Stmt::If(_, t, f) => sets_sorted_in(t) || f.is_some_and(sets_sorted_in),
        _ => false,
    }
}

/// The statements an action runs one after the other.
fn top_level<'a>(s: &'a S<'a>) -> Vec<&'a S<'a>> {
    match s {
        Stmt::Block(v) => v.iter().flat_map(|s| top_level(s)).collect(),
        _ => vec![s],
    }
}

/// Whether running `s` assigns `x` before anything reads it.
fn defines_first(s: &S, x: &str) -> bool {
    let is_x = |e: &E| matches!(e, Expr::Var(n) if *n == x);
    // `split(s, x[, seps])`, whose other arguments do not mention `x`.
    let split_into_x = |e: &E| match e {
        Expr::Call(Either::Left("split"), args) if args.len() >= 2 && is_x(args[1]) => args
            .iter()
            .enumerate()
            .all(|(i, a)| i == 1 || !expr_mentions(a, x)),
        _ => false,
    };
    match s {
        Stmt::Expr(Expr::Assign(lhs, rhs)) if is_x(lhs) => {
            !expr_mentions(rhs, x) || split_into_x(rhs)
        }
        // `n = split(s, x)`, `n += split(s, x)`
        Stmt::Expr(Expr::Assign(lhs, rhs)) | Stmt::Expr(Expr::AssignOp(lhs, _, rhs)) => {
            !expr_mentions(lhs, x) && split_into_x(rhs)
        }
        Stmt::Expr(e) => split_into_x(e),
        Stmt::For(Some(init), ..) => defines_first(init, x),
        Stmt::ForEach(v, arr, _) => *v == x && !expr_mentions(arr, x),
        Stmt::ForRange(v, from, to, _, _) => {
            *v == x && !expr_mentions(from, x) && !expr_mentions(to, x)
        }
        _ => false,
    }
}

fn expr_mentions(e: &E, x: &str) -> bool {
    let mut n = Names::default();
    n.expr(e);
    n.all.contains(x)
}

fn stmt_mentions(s: &S, x: &str) -> bool {
    let mut n = Names::default();
    n.stmt(s);
    n.all.contains(x)
}

/// The variables a piece of code mentions and writes, and whether it does something no
/// parallel run may contain anywhere (read the main input, assign `RS`).
#[derive(Default)]
struct Names<'a> {
    all: HashSet<&'a str>,
    written: HashSet<&'a str>,
    /// Reads the current record: `$n` or `NF`.
    reads_record: bool,
    bad: bool,
}

impl<'a> Names<'a> {
    fn write_target(&mut self, e: &'a E<'a>) {
        match e {
            Expr::Var(n) => {
                self.written.insert(n);
            }
            Expr::Index(arr, _) => self.write_target(arr),
            _ => {}
        }
    }

    fn expr(&mut self, e: &'a E<'a>) {
        match e {
            Expr::ILit(_) | Expr::FLit(_) | Expr::StrLit(_) | Expr::PatLit(_) | Expr::Cond(_) => {}
            Expr::ReadStdin => self.bad = true,
            Expr::Var(n) => {
                self.all.insert(n);
                if *n == "NF" {
                    self.reads_record = true;
                }
            }
            Expr::Unop(op, x) => {
                if *op == Unop::Column {
                    self.reads_record = true;
                }
                self.expr(x)
            }
            Expr::Binop(_, l, r)
            | Expr::Index(l, r)
            | Expr::And(l, r)
            | Expr::Or(l, r)
            | Expr::Nullish(l, r)
            | Expr::Try(l, r) => {
                self.expr(l);
                self.expr(r);
            }
            Expr::Assign(l, r) | Expr::AssignOp(l, _, r) => {
                self.write_target(l);
                self.expr(l);
                self.expr(r);
            }
            Expr::Inc { x, .. } => {
                self.write_target(x);
                self.expr(x);
            }
            Expr::ITE(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            Expr::Getline { into, from, .. } => {
                if from.is_none() {
                    self.bad = true;
                }
                if let Some(into) = into {
                    self.write_target(into);
                    self.expr(into);
                }
                if let Some(from) = from {
                    self.expr(from);
                }
            }
            Expr::Call(f, args) => {
                // The arguments the function assigns.
                let written: &[usize] = match f {
                    Either::Left("split") | Either::Left("patsplit") => &[1, 3],
                    Either::Left("sub") | Either::Left("gsub") | Either::Left("match") => &[2],
                    Either::Right(Function::Delete) | Either::Right(Function::Clear) => &[0],
                    _ => &[],
                };
                for (i, a) in args.iter().enumerate() {
                    if written.contains(&i) {
                        self.write_target(a);
                    }
                    self.expr(a);
                }
                // `sub`/`gsub` without a target and `length` without argument use `$0`.
                if matches!(f, Either::Left("sub") | Either::Left("gsub")) && args.len() < 3
                    || matches!(f, Either::Right(Function::Length)) && args.is_empty()
                {
                    self.reads_record = true;
                }
            }
        }
        if self.written.contains("RS") {
            self.bad = true;
        }
    }

    fn stmt(&mut self, s: &'a S<'a>) {
        match s {
            Stmt::StartCond(_)
            | Stmt::EndCond(_)
            | Stmt::LastCond(_)
            | Stmt::Break
            | Stmt::Continue
            | Stmt::Next
            | Stmt::NextFile => {}
            Stmt::Expr(e) | Stmt::DoWhile(e, _) | Stmt::While(_, e, _) | Stmt::Abort(e) => {
                self.expr(e);
                if let Stmt::DoWhile(_, b) | Stmt::While(_, _, b) = s {
                    self.stmt(b);
                }
            }
            Stmt::Block(v) => v.iter().for_each(|s| self.stmt(s)),
            Stmt::Print(args, out) => {
                args.iter().for_each(|a| self.expr(a));
                if args.is_empty() {
                    self.reads_record = true;
                }
                if let Some((o, _)) = out {
                    self.expr(o);
                }
            }
            Stmt::Printf(fmt, args, out) => {
                self.expr(fmt);
                args.iter().for_each(|a| self.expr(a));
                if let Some((o, _)) = out {
                    self.expr(o);
                }
            }
            Stmt::If(c, t, f) => {
                self.expr(c);
                self.stmt(t);
                if let Some(f) = f {
                    self.stmt(f);
                }
            }
            Stmt::For(init, cond, update, body) => {
                if let Some(init) = init {
                    self.stmt(init);
                }
                if let Some(cond) = cond {
                    self.expr(cond);
                }
                if let Some(update) = update {
                    self.stmt(update);
                }
                self.stmt(body);
            }
            Stmt::ForEach(v, arr, body) => {
                self.all.insert(v);
                self.written.insert(v);
                self.expr(arr);
                self.stmt(body);
            }
            Stmt::ForRange(v, from, to, _, body) => {
                self.all.insert(v);
                self.written.insert(v);
                self.expr(from);
                self.expr(to);
                self.stmt(body);
            }
            Stmt::Switch(e, cases) => {
                self.expr(e);
                for (label, body) in cases.iter() {
                    if let Some(label) = label {
                        self.expr(label);
                    }
                    self.stmt(body);
                }
            }
            Stmt::Return(e) => {
                if let Some(e) = e {
                    self.expr(e);
                }
            }
            Stmt::EndBlock(b) => self.stmt(b),
        }
        if self.written.contains("RS") {
            self.bad = true;
        }
    }
}

/// Walks the main loop: refuses what it may not contain, and sorts the variables it mentions
/// into sums and other mentions.
struct MainVisitor<'a, 'v> {
    user_fns: &'v HashSet<&'a str>,
    ok: bool,
    summed: &'v mut HashSet<&'a str>,
    fraction_summed: &'v mut HashSet<&'a str>,
    /// Variables that hold whole numbers (see `int_vars`).
    ints: &'v HashSet<&'a str>,
    mentioned: &'v mut HashMap<&'a str, HashSet<usize>>,
    /// The index of the action walked, `usize::MAX` for a pattern.
    action: usize,
}

impl<'a, 'v> MainVisitor<'a, 'v> {
    fn mention(&mut self, n: &'a str) {
        self.mentioned.entry(n).or_default().insert(self.action);
    }

    /// `x += e`, `x -= e`, `x++`, `x--` (also on `a[k]`) as a statement: the variable updated
    /// and the expressions read.
    fn sum_update(e: &'a E<'a>) -> Option<(&'a str, Option<&'a E<'a>>, Option<&'a E<'a>>)> {
        let (target, rhs) = match e {
            Expr::AssignOp(t, Binop::Plus | Binop::Minus, r) => (*t, Some(*r)),
            Expr::Inc { x, .. } => (*x, None),
            _ => return None,
        };
        match target {
            Expr::Var(n) => Some((n, None, rhs)),
            Expr::Index(Expr::Var(n), k) => Some((n, Some(*k), rhs)),
            _ => None,
        }
    }

    fn expr(&mut self, e: &'a E<'a>) {
        if !self.ok {
            return;
        }
        match e {
            Expr::ILit(_) | Expr::FLit(_) | Expr::StrLit(_) | Expr::PatLit(_) => {}
            Expr::Cond(_) | Expr::ReadStdin | Expr::Getline { .. } => self.ok = false,
            Expr::Var(n) => self.mention(n),
            Expr::Unop(_, x) => self.expr(x),
            Expr::Binop(_, l, r)
            | Expr::Index(l, r)
            | Expr::And(l, r)
            | Expr::Or(l, r)
            | Expr::Nullish(l, r)
            | Expr::Try(l, r)
            | Expr::Assign(l, r)
            | Expr::AssignOp(l, _, r) => {
                self.expr(l);
                self.expr(r);
            }
            Expr::Inc { x, .. } => self.expr(x),
            Expr::ITE(c, t, f) => {
                self.expr(c);
                self.expr(t);
                self.expr(f);
            }
            Expr::Call(f, args) => {
                let pure = match f {
                    Either::Left(name) => {
                        !self.user_fns.contains(name) && PURE_BUILTINS.contains(name)
                    }
                    Either::Right(f) => matches!(
                        f,
                        Function::Contains | Function::Length | Function::Delete | Function::Clear
                    ),
                };
                if !pure {
                    self.ok = false;
                    return;
                }
                args.iter().for_each(|a| self.expr(a));
            }
        }
    }

    fn stmt(&mut self, s: &'a S<'a>) {
        if !self.ok {
            return;
        }
        match s {
            Stmt::Expr(e) => match Self::sum_update(e) {
                Some((n, k, rhs)) => {
                    self.summed.insert(n);
                    if rhs.is_some_and(|e| !int_valued(e, self.ints)) {
                        self.fraction_summed.insert(n);
                    }
                    if let Some(k) = k {
                        self.expr(k);
                    }
                    if let Some(rhs) = rhs {
                        self.expr(rhs);
                    }
                }
                None => self.expr(e),
            },
            Stmt::Block(v) => v.iter().for_each(|s| self.stmt(s)),
            Stmt::If(c, t, f) => {
                self.expr(c);
                self.stmt(t);
                if let Some(f) = f {
                    self.stmt(f);
                }
            }
            Stmt::For(init, cond, update, body) => {
                if let Some(init) = init {
                    self.stmt(init);
                }
                if let Some(cond) = cond {
                    self.expr(cond);
                }
                if let Some(update) = update {
                    self.stmt(update);
                }
                self.stmt(body);
            }
            Stmt::DoWhile(c, b) | Stmt::While(_, c, b) => {
                self.expr(c);
                self.stmt(b);
            }
            Stmt::ForEach(v, arr, body) => {
                self.mention(v);
                self.expr(arr);
                self.stmt(body);
            }
            Stmt::ForRange(v, from, to, _, body) => {
                self.mention(v);
                self.expr(from);
                self.expr(to);
                self.stmt(body);
            }
            Stmt::Switch(e, cases) => {
                self.expr(e);
                for (label, body) in cases.iter() {
                    if let Some(label) = label {
                        self.expr(label);
                    }
                    self.stmt(body);
                }
            }
            Stmt::Break | Stmt::Continue | Stmt::Next => {}
            Stmt::Print(..)
            | Stmt::Printf(..)
            | Stmt::NextFile
            | Stmt::Return(_)
            | Stmt::Abort(_)
            | Stmt::EndBlock(_)
            | Stmt::StartCond(_)
            | Stmt::EndCond(_)
            | Stmt::LastCond(_) => self.ok = false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::record_parallel_safe as safe;

    #[test]
    fn aggregations_are_parallel() {
        for prog in [
            "{ s += length($3) } END { print s }",
            "{ s += int($3) % 1000 } END { print s }",
            "{ s += length($0); n++ } END { print s / n, NR }",
            "BEGIN { PROCINFO[\"sorted_in\"] = \"@ind_str_asc\" } { a[$2] += $3 > 500 } END { for (k in a) print k, a[k] }",
            "{ a[$2] -= index($3, \"5\") + 1 } END { PROCINFO[\"sorted_in\"] = \"@val_num_desc\"; for (k in a) print k, a[k] }",
            "{ c[$5]++ } END { print length(c) }",
            "/ERROR/ { c++ } END { print c + 0 }",
            "$3 > 500 { hi++ } $3 <= 500 { lo-- } END { print hi, lo }",
            "BEGIN { FS = \",\"; t = 100 } $3 > t { n++ } END { print n }",
            // Per-record temporaries.
            "{ x = $3 * 2; t = length(x); s += length(x) } END { print s }",
            "BEGIN { PROCINFO[\"sorted_in\"] = \"@ind_str_asc\" } { n = split($2, p, \"-\"); c[p[1]] += n } END { for (k in c) print k, c[k] }",
            "BEGIN { PROCINFO[\"sorted_in\"] = \"@ind_str_asc\" } { split($2, p, \"-\"); c[p[1]]++ } END { for (k in c) print k, c[k] }",
            "{ n += split($6, p, \"x\") } END { print n }",
            "{ for (i = 1; i <= NF; i++) s += i * 2 } END { print s }",
            "{ k = length($1); k = k * 2; s += k } END { print s }",
            "{ for (i = 1; i <= NF; i++) s += length($i) } END { print s }",
            "{ if (match($0, /id=[0-9]+/)) ids[substr($0, RSTART, RLENGTH)]++ } END { print length(ids) }",
            "{ local v = $3; if (v > 1) s++ } END { print s }",
            "{ t = 0; t += $3; if (t > 1) s++ } END { print s }",
            "{ gsub(/a/, \"b\"); n += length($0) } END { print n }",
            "NF > 3 { if ($1 == \"x\") next; s += index($2, \"a\") } END { print s }",
            "{ s += 1.0 }\nEND { printf \"%.2f\\n\", s }",
            // `exit` in BEGIN skips the main loop in both modes.
            "BEGIN { exit } { s++ } END { print s }",
        ] {
            assert!(safe(prog, &[]), "{}", prog);
        }
    }

    #[test]
    fn order_or_effects_are_serial() {
        for prog in [
            // Output, input and other effects in the main loop.
            "{ print $1 }",
            "/ERROR/",
            "{ printf \"%s\\n\", $1 }",
            "{ s++ } NR % 1000 == 0 { print s }",
            "{ getline; s += length($1) } END { print s }",
            "{ \"date\" | getline d; s += length($1) } END { print s }",
            "{ system(\"true\"); s += length($1) } END { print s }",
            "{ s += length($1); if (s > 100) exit } END { print s }",
            "{ s += length(rand()) } END { print s }",
            "function g(x) { return x * 2 } { s += length(g($1)) } END { print s }",
            "/a/,/b/ { n++ } END { print n }",
            // Sums of fractions depend on the order of the additions.
            "{ s += $3 } END { print s }",
            "{ a[$2] += $3 } END { for (k in a) print k, a[k] }",
            "{ x = $3 * 2; s += x } END { print s }",
            "{ s += 0.5 } END { print s }",
            // Whole numbers of any magnitude: past 2^53, sums depend on the order too.
            "{ s += int($3) } END { print s }",
            "{ a[$2] += int($3) } END { for (k in a) print k, a[k] }",
            "{ for (i = 1; i <= NF; i++) s += int($i) } END { print s }",
            "{ x = int($3); s += x } END { print s }",
            "{ s += 2 * length($1) * length($2) * length($3) } END { print s }",
            "{ s += 10000000000 } END { print s }",
            "{ s += lshift(1, 40) } END { print s }",
            "{ s += $3 / 2 } END { print s }",
            "{ for (i = 1; i <= NF; i++) s += $i } END { print s }",
            "{ k = length($1); k = k / 2; s += k } END { print s }",
            "{ k = $1; sub(/a/, \"\", k); s += k } END { print s }",
            // Values that depend on the records before.
            "{ m = $1 > m ? $1 : m } END { print m }",
            "{ last = $1 } END { print last }",
            "{ if (s < 100) s += length($1) } END { print s }",
            "!seen[$1]++ { n++ } END { print n }",
            "{ if (!($1 in seen)) { seen[$1] = 1; n++ } } END { print n }",
            "{ a[$1] = $2 } END { for (k in a) print k, a[k] }",
            "{ if (prev != \"\") d += length($1 - prev); prev = $1 } END { print d }",
            "{ s += length(x); x = $1 } END { print s }",
            "{ x = $1 } { s += length(x) } END { print s }",
            "{ s *= $1 } END { print s }",
            "{ s = s + $1 } END { print s }",
            // Per-worker variables.
            "NR == 1 { h = $1 } { s += length($2) } END { print s }",
            "{ s += FNR } END { print s }",
            "{ c[FILENAME]++ } END { for (f in c) print f, c[f] }",
            // An initial value from BEGIN, `-v` or a function is added once per worker.
            "BEGIN { s = 10 } { s += length($1) } END { print s }",
            "function init() { s = 10 } BEGIN { init() } { s += length($1) } END { print s }",
            // The last record in END.
            "{ s += length($1) } END { print s, $0 }",
            "{ s += length($1) } END { print s, NF }",
            // Special variables and RS.
            "{ NF = 2; s += length($1) } END { print s }",
            "BEGIN { RS = \";\" } { s += length($1) } END { print s }",
            "BEGIN { getline h } { s += length($1) } END { print s }",
            "{ s += length($1) } PREPARE { v[PID] = s } END { for (p in v) t += v[p]; print t }",
            // `for (k in a)` in END over an array of the workers: another order than serially.
            "{ a[$2] += $3 > 500 } END { for (k in a) print k, a[k] }",
            "{ x = $1 % 7; c[x]++ } END { for (k in c) print k, c[k] }",
            "{ n[$4]++ } END { if (1) { for (k in n) print k, n[k] } }",
            "{ n[$4]++ } END { for (k in n) print k; PROCINFO[\"sorted_in\"] = \"\" }",
            // Does not parse.
            "{ s += ",
        ] {
            assert!(!safe(prog, &[]), "{}", prog);
        }
        assert!(!safe("{ s += length($1) } END { print s }", &["s"]));
        assert!(safe("{ s += ($1 > k) } END { print s }", &["k"]));
    }
}
