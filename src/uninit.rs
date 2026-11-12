//! Uninitialized numeric variables.
//!
//! In awk an uninitialized value is both 0 and "". zawk types variables statically, so a variable
//! that is only ever assigned numbers is an Int or a Float, and before its first assignment it
//! reads as 0 everywhere: `print "[" cnt "]"; cnt++` printed `[0]` and `cnt == ""` was false.
//!
//! This module finds the global variables and arrays for which the difference is visible: ones
//! that are only assigned numbers, and that are read where a string is expected (see
//! [`string_operands`]). For such a variable `x`, the cfg module keeps a hidden flag `%def:x`
//! that is set by every assignment to `x`, and those reads produce "" while it is unset. Reading
//! an element of such an array produces "" when the element does not exist.
//!
//! A variable assigned an element of such an array (`b = a[k]`) is uninitialized when the element
//! did not exist. A user-defined function whose result is read as a string, and that may return
//! without a value, gets a flag `%ret:f` that tells whether its last call returned one.
//!
//! Variables that may be assigned in other ways (getline, sub/gsub, for-in, -v) are left alone,
//! as are function parameters.
use crate::arena::Arena;
use crate::ast::{Binop, Expr, FunDec, Stmt};
use crate::builtins::{Function, Variable};
use crate::common::{Either, Stage};
use hashbrown::{HashMap, HashSet};
use std::hash::Hash;

#[derive(Debug)]
pub(crate) struct Uninit<I> {
    /// Tracked global variables, mapped to the name of their "assigned" flag: numeric variables
    /// whose uninitialized value may be read as a string, and variables passed to `typeof`.
    pub vars: HashMap<I, I>,
    /// Tracked global arrays: their missing elements read as "" where a string is expected.
    /// Each is mapped to the name of a hidden array of the elements created by a read and not
    /// assigned since, which keep reading as "".
    pub maps: HashMap<I, I>,
    /// Tracked user-defined functions, mapped to the name of their "returned a value" flag.
    pub funcs: HashMap<I, I>,
    /// Variables read as a value somewhere (other than by `typeof`, `isarray` or as an argument
    /// of a user-defined function), keyed by the function they are a parameter of (`None` for
    /// globals). gawk's `typeof` reports a variable that was never read as "untyped" rather
    /// than "unassigned"; zawk approximates "before the call" with "anywhere in the program".
    pub scalar_refs: HashSet<(Option<I>, I)>,
}

impl<I> Default for Uninit<I> {
    fn default() -> Self {
        Uninit {
            vars: Default::default(),
            maps: Default::default(),
            funcs: Default::default(),
            scalar_refs: Default::default(),
        }
    }
}

/// The operands of `e` that are read as strings: both operands of a concatenation, the operand
/// compared with a string (a string literal or a concatenation), and the left side of `~`. The
/// arguments of print, of length() and of other builtins taking strings (see
/// [`builtin_str_arg`]), the `%s` arguments of printf and sprintf, and array elements used as
/// subscripts are also read as strings.
pub(crate) fn string_operands<'a, 'b, I>(
    e: &'a Expr<'a, 'b, I>,
) -> (Option<&'a Expr<'a, 'b, I>>, Option<&'a Expr<'a, 'b, I>>) {
    use Binop::*;
    fn is_str<I>(e: &Expr<I>) -> bool {
        matches!(e, Expr::StrLit(_) | Expr::Binop(Binop::Concat, _, _))
    }
    match e {
        Expr::Binop(Concat, l, r) => (Some(l), Some(r)),
        Expr::Binop(IsMatch, l, _) => (Some(l), None),
        Expr::Binop(LT | GT | LTE | GTE | EQ, l, r) => (
            if is_str(r) { Some(l) } else { None },
            if is_str(l) { Some(r) } else { None },
        ),
        _ => (None, None),
    }
}

/// Whether `e` may have a string value, judging by its form alone: literal numbers, arithmetic
/// and comparisons cannot.
pub(crate) fn may_be_str<I: std::fmt::Display>(e: &Expr<I>) -> bool {
    use Expr::*;
    match e {
        ILit(_) | FLit(_) | PatLit(_) | Cond(_) | Inc { .. } | AssignOp(..) => false,
        And(..) | Or(..) | Getline { .. } | ReadStdin => false,
        Unop(op, _) => matches!(op, crate::ast::Unop::Column),
        Binop(op, _, _) => matches!(op, crate::ast::Binop::Concat),
        Var(i) => !crate::ast::is_uninit_var(&i.to_string()),
        _ => true,
    }
}

/// For a comparison that is not already a string comparison (see [`string_operands`]), the
/// operands that are compared with a value that may be a string: an uninitialized one compares as
/// "" with a string and as 0 with a number, so its "assigned" flag decides at runtime.
pub(crate) fn uninit_cmp_operands<'a, 'b, I: std::fmt::Display>(
    e: &'a Expr<'a, 'b, I>,
) -> (Option<&'a Expr<'a, 'b, I>>, Option<&'a Expr<'a, 'b, I>>) {
    use Binop::*;
    match e {
        Expr::Binop(LT | GT | LTE | GTE | EQ, l, r) if matches!(string_operands(e), (None, None)) => (
            if may_be_str(r) { Some(l) } else { None },
            if may_be_str(l) { Some(r) } else { None },
        ),
        _ => (None, None),
    }
}

/// The arguments (after the format) of a printf or sprintf call with the format `fmt` that are
/// printed with `%s`: those read as strings. `None` when the format is not a literal, or uses
/// positional arguments (`%1$s`).
pub(crate) fn printf_str_args<I>(fmt: &Expr<I>, nargs: usize) -> Option<Vec<bool>> {
    let fmt = match fmt {
        Expr::StrLit(s) => *s,
        _ => return None,
    };
    let mut res = vec![false; nargs];
    let mut arg = 0;
    let mut i = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        i += 1;
        if fmt.get(i) == Some(&b'%') {
            i += 1;
            continue;
        }
        // Flags, width and precision; `*` takes an argument.
        while let Some(&c) = fmt.get(i) {
            match c {
                b'$' => return None,
                b'*' => arg += 1,
                b'-' | b'+' | b' ' | b'#' | b'.' | b'0'..=b'9' => {}
                _ => break,
            }
            i += 1;
        }
        if fmt.get(i) == Some(&b's') && arg < nargs {
            res[arg] = true;
        }
        arg += 1;
        i += 1;
    }
    Some(res)
}

/// Whether argument `i` of a call to the builtin `f` with `args` is read as a string (other
/// than by `length`, `typeof` and as an array subscript, which are handled on their own).
pub(crate) fn builtin_str_arg<I>(f: Function, args: &[&Expr<I>], i: usize) -> bool {
    use Function::*;
    if matches!(
        f,
        Length | TypeOfVariable | TypeOfUntyped | IsArray | Contains | Delete | IncMap | Clear
    ) {
        return false;
    }
    f.str_param(i, args.len())
}

/// Builtins whose result is a number.
fn numeric_builtin(f: Function) -> bool {
    use Function::*;
    matches!(
        f,
        Length
            | Strlen
            | SubstrIndex
            | ToInt
            | ToFloat
            | FloatFunc(_)
            | IntFunc(_)
            | Rand
            | Srand
            | System
            | Close
            | Fflush
            | Match
            | Split
            | PatSplit
            | Sub
            | GSub
            | Contains
    )
}

#[derive(Clone)]
enum Target<I> {
    Var(I),
    Map(I),
}

struct Analysis<'a, 'b, I> {
    udfs: HashSet<I>,
    assigned_vars: HashSet<I>,
    bad_vars: HashSet<I>,
    // Global variables assigned in ways that do not set their "assigned" flag (getline, sub/gsub,
    // for-in): they cannot be tracked. A variable that is only assigned non-numeric values by
    // ordinary assignments can: each assignment sets the flag (see `cfg::mark_assigned`).
    untracked_vars: HashSet<I>,
    // Global variables assigned values that may be arrays (the result of a call): not tracked.
    nonscalar_vars: HashSet<I>,
    str_vars: HashSet<I>,
    assigned_maps: HashSet<I>,
    bad_maps: HashSet<I>,
    str_maps: HashSet<I>,
    // Global variables used as arrays (`x[k]`, `k in x`, `for (k in x)`, `delete x`): a value
    // copied into one (`x = y`) is an array, which has no "uninitialized" string value.
    array_vars: HashSet<I>,
    // Assignments of possibly non-numeric values, with the parameters in scope.
    assigns: Vec<(Target<I>, &'a Expr<'a, 'b, I>, &'a [I])>,
    // User-defined functions whose result is read as a string, functions with a `return` without
    // a value, and the values returned by each function (with its parameters).
    str_funcs: HashSet<I>,
    bare_return_funcs: HashSet<I>,
    returns: Vec<(I, &'a Expr<'a, 'b, I>, &'a [I])>,
    // Functions that may return a value that is not a number.
    bad_funcs: HashSet<I>,
    // The function being analyzed (`None` for the main program), and the variables read so far.
    cur_fn: Option<I>,
    scalar_refs: HashSet<(Option<I>, I)>,
    // Global variables passed to `typeof`: they get an "assigned" flag, so that `typeof` tells
    // at runtime whether they have been assigned yet.
    typeof_vars: HashSet<I>,
}

impl<'a, 'b, I> Analysis<'a, 'b, I>
where
    I: Hash + Eq + Clone + std::fmt::Display,
    Variable: TryFrom<I>,
    Function: TryFrom<I>,
{
    /// A global (not a parameter, special or hidden) variable named `i`.
    fn global(&self, i: &I, params: &[I]) -> bool {
        !params.contains(i)
            && Variable::try_from(i.clone()).is_err()
            && !i.to_string().starts_with(['%', '-'])
    }

    fn global_var(&self, e: &Expr<'a, 'b, I>, params: &[I]) -> Option<I> {
        match e {
            Expr::Var(i) if self.global(i, params) => Some(i.clone()),
            _ => None,
        }
    }

    fn global_map(&self, e: &Expr<'a, 'b, I>, params: &[I]) -> Option<I> {
        match e {
            Expr::Index(arr, _) => self.global_var(arr, params),
            _ => None,
        }
    }

    fn builtin(&self, f: &Either<I, Function>) -> Option<Function> {
        match f {
            Either::Right(bi) => Some(*bi),
            Either::Left(name) if !self.udfs.contains(name) => {
                Function::try_from(name.clone()).ok()
            }
            Either::Left(_) => None,
        }
    }

    /// Whether `e` is a number, given that the variables in `bad_vars` may not be.
    fn is_num(&self, e: &Expr<'a, 'b, I>, params: &[I]) -> bool {
        use Expr::*;
        match e {
            ILit(_) | FLit(_) | PatLit(_) | Cond(_) | Inc { .. } | AssignOp(..) => true,
            And(..) | Or(..) | Getline { .. } | ReadStdin => true,
            Unop(op, _) => !matches!(op, crate::ast::Unop::Column),
            Binop(op, _, _) => !matches!(op, crate::ast::Binop::Concat),
            Assign(_, e) => self.is_num(e, params),
            ITE(_, t, f) => self.is_num(t, params) && self.is_num(f, params),
            Nullish(l, r) | Try(l, r) => self.is_num(l, params) && self.is_num(r, params),
            // The value of `local x`.
            Var(i) if crate::ast::is_uninit_var(&i.to_string()) => true,
            Var(i) => self.global(i, params) && !self.bad_vars.contains(i),
            // An element of an array only assigned numbers.
            Index(..) => self
                .global_map(e, params)
                .is_some_and(|m| !self.bad_maps.contains(&m)),
            Call(Either::Left(f), _) if self.udfs.contains(f) => !self.bad_funcs.contains(f),
            Call(f, _) => self.builtin(f).is_some_and(numeric_builtin),
            StrLit(_) => false,
        }
    }

    fn string_read(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if let Some(v) = self.global_var(e, params) {
            self.str_vars.insert(v);
        } else if let Some(m) = self.global_map(e, params) {
            self.str_maps.insert(m);
        } else if let Expr::Call(Either::Left(f), _) = e {
            if self.udfs.contains(f) {
                self.str_funcs.insert(f.clone());
            }
        }
    }

    /// `e` is assigned in a way that is not tracked.
    fn untracked_assign(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if let Some(v) = self.global_var(e, params) {
            self.assigned_vars.insert(v.clone());
            self.bad_vars.insert(v.clone());
            self.untracked_vars.insert(v);
        } else if let Some(m) = self.global_map(e, params) {
            self.bad_maps.insert(m);
        }
    }

    fn assign(
        &mut self,
        lhs: &'a Expr<'a, 'b, I>,
        rhs: Option<&'a Expr<'a, 'b, I>>,
        params: &'a [I],
    ) {
        let target = if let Some(v) = self.global_var(lhs, params) {
            self.assigned_vars.insert(v.clone());
            Target::Var(v)
        } else if let Some(m) = self.global_map(lhs, params) {
            self.assigned_maps.insert(m.clone());
            Target::Map(m)
        } else {
            return;
        };
        // `x = c ? t : f` assigns `t` or `f` (see `cfg`, which converts it as
        // `c ? (x = t) : (x = f)`): record each branch as an assignment of its own.
        fn leaves<'a, 'b, I>(e: &'a Expr<'a, 'b, I>, out: &mut Vec<&'a Expr<'a, 'b, I>>) {
            match e {
                Expr::ITE(_, t, f) => {
                    leaves(t, out);
                    leaves(f, out);
                }
                _ => out.push(e),
            }
        }
        if let Some(rhs) = rhs {
            let mut rhss = Vec::new();
            leaves(rhs, &mut rhss);
            for rhs in rhss {
                self.assigns.push((target.clone(), rhs, params));
            }
        }
    }

    fn array_use(&mut self, arr: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if let Some(v) = self.global_var(arr, params) {
            self.array_vars.insert(v);
        }
    }

    fn scalar_ref(&mut self, i: &I, params: &[I]) {
        let func = if params.contains(i) {
            self.cur_fn.clone()
        } else {
            None
        };
        self.scalar_refs.insert((func, i.clone()));
    }

    fn expr(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        use Expr::*;
        let (l, r) = string_operands(e);
        for operand in [l, r].into_iter().flatten() {
            self.string_read(operand, params);
        }
        // Array elements compared with a possible string are left alone: their reads are not
        // guarded (see `cfg::uninit_cmp`).
        let (l, r) = uninit_cmp_operands(e);
        for operand in [l, r].into_iter().flatten() {
            if !matches!(operand, Index(..)) {
                self.string_read(operand, params);
            }
        }
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | Cond(_) | ReadStdin => {}
            Var(i) => self.scalar_ref(i, params),
            Unop(_, e) => self.expr(e, params),
            Index(arr, ix) => {
                self.array_use(arr, params);
                self.subscript(ix, params);
                self.expr(arr, params);
                self.expr(ix, params);
            }
            Binop(_, l, r) | And(l, r) | Or(l, r) | Nullish(l, r) | Try(l, r) => {
                self.expr(l, params);
                self.expr(r, params);
            }
            ITE(c, t, f) => {
                self.expr(c, params);
                self.expr(t, params);
                self.expr(f, params);
            }
            Assign(lhs, rhs) => {
                self.assign(lhs, Some(rhs), params);
                self.lvalue(lhs, params);
                self.expr(rhs, params);
            }
            AssignOp(lhs, _, rhs) => {
                self.assign(lhs, None, params);
                self.lvalue(lhs, params);
                self.expr(rhs, params);
            }
            Inc { x, .. } => {
                self.assign(x, None, params);
                self.lvalue(x, params);
            }
            Getline { into, from, .. } => {
                if let Some(into) = into {
                    self.untracked_assign(into, params);
                    self.lvalue(into, params);
                }
                if let Some(from) = from {
                    self.expr(from, params);
                }
            }
            Call(f, args) => {
                let bi = self.builtin(f);
                if let (Some(Function::Length), [arg]) = (bi, args) {
                    self.string_read(arg, params);
                }
                match bi {
                    Some(bi) => {
                        for (i, a) in args.iter().enumerate() {
                            if builtin_str_arg(bi, args, i) {
                                self.string_read(a, params);
                            }
                        }
                    }
                    None if matches!(f, Either::Left(name) if name.to_string() == "sprintf") => {
                        if let [fmt, rest @ ..] = &args[..] {
                            self.printf_args(fmt, rest, params);
                        }
                    }
                    None => {}
                }
                if let (Some(Function::Contains | Function::Delete), [_, ix]) = (bi, args) {
                    self.subscript(ix, params);
                }
                match (bi, args) {
                    (Some(Function::Contains), [arr, _]) => self.array_use(arr, params),
                    (Some(Function::Delete | Function::Clear), [arr, ..]) => {
                        self.array_use(arr, params)
                    }
                    _ => {}
                }
                if let (Some(Function::Sub | Function::GSub), [_, _, target, ..]) = (bi, args) {
                    self.untracked_assign(target, params);
                }
                // An array passed to a function may be assigned there.
                let keeps_arrays = matches!(
                    bi,
                    Some(
                        Function::Length | Function::Contains | Function::Delete | Function::Clear
                    )
                );
                // typeof, isarray and user-defined functions do not read a variable passed to them
                // (in gawk it stays "untyped").
                let inspects =
                    match f {
                        Either::Left(name) => self.udfs.contains(name),
                        Either::Right(_) => false,
                    } || matches!(bi, Some(Function::TypeOfVariable | Function::IsArray));
                if let (Some(Function::TypeOfVariable), [a]) = (bi, args) {
                    if let Some(v) = self.global_var(a, params) {
                        self.typeof_vars.insert(v);
                    }
                }
                for a in args.iter() {
                    if !keeps_arrays {
                        if let Some(m) = self.global_var(a, params) {
                            self.bad_maps.insert(m);
                        }
                    }
                    match a {
                        Var(_) if inspects => {}
                        Index(Var(_), ix) if inspects => self.expr(ix, params),
                        _ => self.expr(a, params),
                    }
                }
            }
        }
    }

    /// An array subscript: an element of a tracked array used there is read as a string
    /// (`b[a[k]]`). Variables are not, so that integer keys (`a[i]`) stay integers.
    fn subscript(&mut self, ix: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if self.global_map(ix, params).is_some() {
            self.string_read(ix, params);
        }
    }

    /// The `%s` arguments of printf and sprintf with a literal format are read as strings.
    fn printf_args(
        &mut self,
        fmt: &'a Expr<'a, 'b, I>,
        args: &'a [&'a Expr<'a, 'b, I>],
        params: &'a [I],
    ) {
        if let Some(strs) = printf_str_args(fmt, args.len()) {
            for (a, is_str) in args.iter().zip(strs) {
                if is_str {
                    self.string_read(a, params);
                }
            }
        }
    }

    /// The subexpressions of an assigned expression.
    fn lvalue(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        match e {
            Expr::Index(arr, ix) => {
                self.array_use(arr, params);
                self.subscript(ix, params);
                self.expr(arr, params);
                self.expr(ix, params);
            }
            Expr::Unop(_, e) => self.expr(e, params),
            _ => {}
        }
    }

    fn stmt(&mut self, s: &'a Stmt<'a, 'b, I>, params: &'a [I]) {
        use Stmt::*;
        match s {
            StartCond(_) | EndCond(_) | LastCond(_) | Break | Continue | Next | NextFile => {}
            Expr(e) => self.expr(e, params),
            Block(stmts) => {
                for s in stmts.iter() {
                    self.stmt(s, params);
                }
            }
            Print(args, out) => {
                for a in args.iter() {
                    self.string_read(a, params);
                    self.expr(a, params);
                }
                if let Some((out, _)) = out {
                    self.expr(out, params);
                }
            }
            Printf(fmt, args, out) => {
                self.printf_args(fmt, args, params);
                self.expr(fmt, params);
                for a in args.iter() {
                    self.expr(a, params);
                }
                if let Some((out, _)) = out {
                    self.expr(out, params);
                }
            }
            If(c, t, f) => {
                self.expr(c, params);
                self.stmt(t, params);
                if let Some(f) = f {
                    self.stmt(f, params);
                }
            }
            For(init, cond, update, body) => {
                for s in init.iter().chain(update.iter()) {
                    self.stmt(s, params);
                }
                if let Some(c) = cond {
                    self.expr(c, params);
                }
                self.stmt(body, params);
            }
            DoWhile(c, body) | While(_, c, body) => {
                self.expr(c, params);
                self.stmt(body, params);
            }
            ForEach(v, arr, body) => {
                if self.global(v, params) {
                    self.assigned_vars.insert(v.clone());
                    self.bad_vars.insert(v.clone());
                    self.untracked_vars.insert(v.clone());
                }
                self.array_use(arr, params);
                self.expr(arr, params);
                self.stmt(body, params);
            }
            ForRange(v, start, end, _, body) => {
                // The loop variable is only assigned integers.
                if self.global(v, params) {
                    self.assigned_vars.insert(v.clone());
                }
                self.expr(start, params);
                self.expr(end, params);
                self.stmt(body, params);
            }
            Abort(e) => self.expr(e, params),
            Return(e) => {
                let cur_fn = self.cur_fn.clone();
                match (e, cur_fn) {
                    (Some(e), Some(f)) => self.returns.push((f, e, params)),
                    (None, Some(f)) => {
                        self.bare_return_funcs.insert(f);
                    }
                    _ => {}
                }
                if let Some(e) = e {
                    self.expr(e, params);
                }
            }
            EndBlock(s) => self.stmt(s, params),
            Switch(e, cases) => {
                self.expr(e, params);
                // A variable is compared with string and regex case labels as a string.
                let str_case = cases.iter().any(|(l, _)| {
                    matches!(
                        l,
                        Some(crate::ast::Expr::StrLit(_) | crate::ast::Expr::PatLit(_))
                    )
                });
                if str_case && matches!(e, crate::ast::Expr::Var(_)) {
                    self.string_read(e, params);
                }
                for (_, body) in cases.iter() {
                    self.stmt(body, params);
                }
            }
        }
    }

    /// Whether `e` is a scalar (not an array): a number, a string literal, a concatenation, a
    /// field, an array element or a global variable that is not an array. A function may return
    /// an array (`r = json_flatten(s)`), and a variable that may hold one is not tracked.
    fn is_scalar(&self, e: &Expr<'a, 'b, I>, params: &[I]) -> bool {
        use Expr::*;
        match e {
            StrLit(_) | Index(..) => true,
            Unop(crate::ast::Unop::Column, _) | Binop(crate::ast::Binop::Concat, _, _) => true,
            Assign(_, e) => self.is_scalar(e, params),
            ITE(_, t, f) => self.is_scalar(t, params) && self.is_scalar(f, params),
            Var(i) if self.global(i, params) => {
                !self.array_vars.contains(i) && !self.nonscalar_vars.contains(i)
            }
            _ => self.is_num(e, params),
        }
    }

    /// Disqualify the variables and arrays assigned values that may not be numbers, until no
    /// more change (a variable assigned another variable depends on it).
    fn solve(&mut self) {
        loop {
            let mut changed = false;
            for (target, rhs, params) in self.assigns.iter() {
                if self.is_num(rhs, params) {
                    continue;
                }
                changed |= match target {
                    Target::Var(v) => {
                        let nonscalar = !self.is_scalar(rhs, params)
                            && self.nonscalar_vars.insert(v.clone());
                        self.bad_vars.insert(v.clone()) | nonscalar
                    }
                    Target::Map(m) => self.bad_maps.insert(m.clone()),
                };
            }
            for (f, e, params) in self.returns.iter() {
                if !self.is_num(e, params) {
                    changed |= self.bad_funcs.insert(f.clone());
                }
            }
            if !changed {
                break;
            }
        }
    }
}

pub(crate) fn analyze<'s, 'a, 'b, I>(
    arena: &'a Arena,
    decs: &'s [FunDec<'s, 'b, I>],
    main: &Stage<&'s Stmt<'s, 'b, I>>,
) -> Uninit<I>
where
    I: Hash + Eq + Clone + std::fmt::Display + From<&'a str>,
    Variable: TryFrom<I>,
    Function: TryFrom<I>,
{
    let mut a = Analysis {
        udfs: decs.iter().map(|d| d.name.clone()).collect(),
        assigned_vars: Default::default(),
        bad_vars: Default::default(),
        untracked_vars: Default::default(),
        nonscalar_vars: Default::default(),
        str_vars: Default::default(),
        assigned_maps: Default::default(),
        bad_maps: Default::default(),
        str_maps: Default::default(),
        array_vars: Default::default(),
        assigns: Default::default(),
        str_funcs: Default::default(),
        bare_return_funcs: Default::default(),
        returns: Default::default(),
        bad_funcs: Default::default(),
        cur_fn: None,
        scalar_refs: Default::default(),
        typeof_vars: Default::default(),
    };
    for dec in decs.iter() {
        a.cur_fn = Some(dec.name.clone());
        a.stmt(dec.body, &dec.args[..]);
    }
    a.cur_fn = None;
    for s in main.iter() {
        a.stmt(s, &[]);
    }
    a.solve();
    // A variable copied into one read as a string (`c = b`) is read as a string too. A copied
    // variable that is never assigned is tracked as well (its flag stays unset), so that a copy
    // whose static type is a number (`c = 0 ? 3 : b`) is uninitialized too.
    let mut copied: HashSet<I> = HashSet::new();
    loop {
        let mut changed = false;
        for (target, rhs, params) in a.assigns.iter() {
            if let (Target::Var(v), Some(y)) = (target, a.global_var(rhs, params)) {
                if a.str_vars.contains(v) {
                    changed |= a.str_vars.insert(y.clone());
                    copied.insert(y);
                }
            }
        }
        if !changed {
            break;
        }
    }
    let vars: HashMap<I, I> = a
        .assigned_vars
        .iter()
        .chain(copied.iter().filter(|y| !a.assigned_vars.contains(*y)))
        .filter(|v| {
            // A variable also assigned (scalar) strings is tracked too: the flag is set by every
            // assignment, so its reads before the first one are "" whatever later values are.
            ((a.str_vars.contains(*v)
                && !a.untracked_vars.contains(*v)
                && !a.nonscalar_vars.contains(*v))
                || a.typeof_vars.contains(*v))
                && !a.array_vars.contains(*v)
        })
        .map(|v| {
            let flag: &'a str = arena.alloc_str(&format!("%def:{}", v));
            (v.clone(), I::from(flag))
        })
        .collect();
    // A tracked variable assigned an array element or the result of a function reads the
    // element or the result as a string.
    for (target, rhs, _) in a.assigns.iter() {
        match (target, rhs) {
            (Target::Var(v), Expr::Index(Expr::Var(m), _)) if vars.contains_key(v) => {
                a.str_maps.insert(m.clone());
            }
            (Target::Var(v), Expr::Call(Either::Left(f), _))
                if vars.contains_key(v) && a.udfs.contains(f) =>
            {
                a.str_funcs.insert(f.clone());
            }
            _ => {}
        }
    }
    let maps = a
        .assigned_maps
        .iter()
        .filter(|m| a.str_maps.contains(*m) && !a.bad_maps.contains(*m))
        .map(|m| {
            let undef: &'a str = arena.alloc_str(&format!("%undef:{}", m));
            (m.clone(), I::from(undef))
        })
        .collect();
    // Functions that always end with `return <value>` need no flag.
    fn ends_with_return<'a, 'b, I>(s: &Stmt<'a, 'b, I>) -> bool {
        match s {
            Stmt::Return(Some(_)) => true,
            Stmt::Block(stmts) => stmts.last().is_some_and(|s| ends_with_return(s)),
            Stmt::If(_, t, Some(f)) => ends_with_return(t) && ends_with_return(f),
            _ => false,
        }
    }
    let funcs = decs
        .iter()
        .filter(|d| {
            a.str_funcs.contains(&d.name)
                && (a.bare_return_funcs.contains(&d.name) || !ends_with_return(d.body))
        })
        .map(|d| {
            let flag: &'a str = arena.alloc_str(&format!("%ret:{}", d.name));
            (d.name.clone(), I::from(flag))
        })
        .collect();
    Uninit {
        vars,
        maps,
        funcs,
        scalar_refs: a.scalar_refs,
    }
}
