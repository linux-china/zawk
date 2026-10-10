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
    /// Tracked global variables, mapped to the name of their "assigned" flag.
    pub vars: HashMap<I, I>,
    /// Tracked global arrays: their missing elements read as "" where a string is expected.
    pub maps: HashSet<I>,
}

impl<I> Default for Uninit<I> {
    fn default() -> Self {
        Uninit {
            vars: Default::default(),
            maps: Default::default(),
        }
    }
}

/// The operands of `e` that are read as strings: both operands of a concatenation, and the
/// operand compared with a string (a string literal or a concatenation). The arguments of print
/// and of length() are also read as strings.
pub(crate) fn string_operands<'a, 'b, I>(
    e: &'a Expr<'a, 'b, I>,
) -> (Option<&'a Expr<'a, 'b, I>>, Option<&'a Expr<'a, 'b, I>>) {
    use Binop::*;
    fn is_str<I>(e: &Expr<I>) -> bool {
        matches!(e, Expr::StrLit(_) | Expr::Binop(Binop::Concat, _, _))
    }
    match e {
        Expr::Binop(Concat, l, r) => (Some(l), Some(r)),
        Expr::Binop(LT | GT | LTE | GTE | EQ, l, r) => (
            if is_str(r) { Some(l) } else { None },
            if is_str(l) { Some(r) } else { None },
        ),
        _ => (None, None),
    }
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
            | Sub
            | GSub
            | Contains
    )
}

enum Target<I> {
    Var(I),
    Map(I),
}

struct Analysis<'a, 'b, I> {
    udfs: HashSet<I>,
    assigned_vars: HashSet<I>,
    bad_vars: HashSet<I>,
    str_vars: HashSet<I>,
    assigned_maps: HashSet<I>,
    bad_maps: HashSet<I>,
    str_maps: HashSet<I>,
    // Assignments of possibly non-numeric values, with the parameters in scope.
    assigns: Vec<(Target<I>, &'a Expr<'a, 'b, I>, &'a [I])>,
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
            Either::Left(name) if !self.udfs.contains(name) => Function::try_from(name.clone()).ok(),
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
            Var(i) => self.global(i, params) && !self.bad_vars.contains(i),
            Call(f, _) => self.builtin(f).is_some_and(numeric_builtin),
            StrLit(_) | Index(..) => false,
        }
    }

    fn string_read(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if let Some(v) = self.global_var(e, params) {
            self.str_vars.insert(v);
        } else if let Some(m) = self.global_map(e, params) {
            self.str_maps.insert(m);
        }
    }

    /// `e` is assigned in a way that is not tracked.
    fn untracked_assign(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        if let Some(v) = self.global_var(e, params) {
            self.bad_vars.insert(v);
        } else if let Some(m) = self.global_map(e, params) {
            self.bad_maps.insert(m);
        }
    }

    fn assign(&mut self, lhs: &'a Expr<'a, 'b, I>, rhs: Option<&'a Expr<'a, 'b, I>>, params: &'a [I]) {
        let target = if let Some(v) = self.global_var(lhs, params) {
            self.assigned_vars.insert(v.clone());
            Target::Var(v)
        } else if let Some(m) = self.global_map(lhs, params) {
            self.assigned_maps.insert(m.clone());
            Target::Map(m)
        } else {
            return;
        };
        if let Some(rhs) = rhs {
            self.assigns.push((target, rhs, params));
        }
    }

    fn expr(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        use Expr::*;
        let (l, r) = string_operands(e);
        for operand in [l, r].into_iter().flatten() {
            self.string_read(operand, params);
        }
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | Var(_) | Cond(_) | ReadStdin => {}
            Unop(_, e) => self.expr(e, params),
            Binop(_, l, r) | Index(l, r) | And(l, r) | Or(l, r) => {
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
                if let (Some(Function::Sub | Function::GSub), [_, _, target, ..]) = (bi, args) {
                    self.untracked_assign(target, params);
                }
                // An array passed to a function may be assigned there.
                let keeps_arrays = matches!(
                    bi,
                    Some(Function::Length | Function::Contains | Function::Delete | Function::Clear)
                );
                for a in args.iter() {
                    if !keeps_arrays {
                        if let Some(m) = self.global_var(a, params) {
                            self.bad_maps.insert(m);
                        }
                    }
                    self.expr(a, params);
                }
            }
        }
    }

    /// The subexpressions of an assigned expression.
    fn lvalue(&mut self, e: &'a Expr<'a, 'b, I>, params: &'a [I]) {
        match e {
            Expr::Index(arr, ix) => {
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
                    self.bad_vars.insert(v.clone());
                }
                self.expr(arr, params);
                self.stmt(body, params);
            }
            Return(e) => {
                if let Some(e) = e {
                    self.expr(e, params);
                }
            }
            EndBlock(s) => self.stmt(s, params),
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
                    Target::Var(v) => self.bad_vars.insert(v.clone()),
                    Target::Map(m) => self.bad_maps.insert(m.clone()),
                };
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
        str_vars: Default::default(),
        assigned_maps: Default::default(),
        bad_maps: Default::default(),
        str_maps: Default::default(),
        assigns: Default::default(),
    };
    for dec in decs.iter() {
        a.stmt(dec.body, &dec.args[..]);
    }
    for s in main.iter() {
        a.stmt(s, &[]);
    }
    a.solve();
    let vars = a
        .assigned_vars
        .iter()
        .filter(|v| a.str_vars.contains(*v) && !a.bad_vars.contains(*v))
        .map(|v| {
            let flag: &'a str = arena.alloc_str(&format!("%def:{}", v));
            (v.clone(), I::from(flag))
        })
        .collect();
    let maps = a
        .assigned_maps
        .iter()
        .filter(|m| a.str_maps.contains(*m) && !a.bad_maps.contains(*m))
        .cloned()
        .collect();
    Uninit { vars, maps }
}
