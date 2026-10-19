use crate::arena;
use crate::ast::{self, Expr, Stmt, Unop};
use crate::builtins::{self, FromStatic, IsSprintf};
use crate::common::{Either, FileSpec, Graph, NodeIx, NumTy, Result, Stage};
use crate::dom;
use crate::lexer::Loc;
use crate::uninit;

use hashbrown::{HashMap, HashSet};
use petgraph::Direction;
use smallvec::smallvec; // macro

use std::collections::VecDeque;
use std::convert::TryFrom;
use std::fmt;
use std::hash::Hash;
use std::io;
use std::mem;
use crate::runtime::{Int};

pub(crate) type SmallVec<T> = smallvec::SmallVec<[T; 4]>;

#[derive(Debug, Eq, PartialEq, Hash)]
pub(crate) enum FunctionName<I> {
    Begin,
    MainLoop,
    End,
    Named(I),
}

impl<I: fmt::Display> fmt::Display for FunctionName<I> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let name = match self {
            FunctionName::Begin => "<begin>",
            FunctionName::MainLoop => "<main>",
            FunctionName::End => "<end>",
            FunctionName::Named(i) => return write!(f, "{}", i),
        };
        write!(f, "{}", name)
    }
}

impl<I> FunctionName<I> {
    fn is_main(&self) -> bool {
        // TODO: does the slot mechanism mean that we can have this hold for any not-named
        // FunctionName?
        matches!(self, FunctionName::MainLoop)
    }
}

#[derive(Debug, Default)]
pub(crate) struct BasicBlock<'a> {
    pub q: VecDeque<PrimStmt<'a>>,
    pub sealed: bool,
}

// None indicates `else`
#[derive(Debug, Default)]
pub(crate) struct Transition<'a>(pub Option<PrimVal<'a>>);

impl<'a> Transition<'a> {
    fn new(pv: PrimVal<'a>) -> Transition<'a> {
        Transition(Some(pv))
    }
    fn null() -> Transition<'a> {
        Transition(None)
    }
}

pub(crate) type Cfg<'a> = Graph<BasicBlock<'a>, Transition<'a>>;

fn dbg_print(cfg: &Cfg, w: &mut impl io::Write) -> io::Result<()> {
    for (i, n) in cfg.raw_nodes().iter().enumerate() {
        writeln!(w, "{}:", i)?;
        for s in n.weight.q.iter() {
            writeln!(w, "\t{}", s)?;
        }
        let mut walker = cfg.neighbors(NodeIx::new(i)).detach();
        let mut sv = SmallVec::new();
        while let Some((t_ix, n_ix)) = walker.next(cfg) {
            sv.push((t_ix, n_ix));
        }
        sv.reverse();
        for (t_ix, n_ix) in sv.into_iter() {
            let trans = cfg.edge_weight(t_ix).unwrap();
            match trans {
                Transition(Some(t)) => {
                    writeln!(w, "\tbrif {} :{}", t, n_ix.index())?;
                }
                Transition(None) => writeln!(w, "\tbr :{}", n_ix.index())?,
            }
        }
    }
    Ok(())
}

#[derive(Copy, Clone, Hash, Eq, PartialEq, Debug)]
pub(crate) struct Ident {
    pub(crate) low: NumTy,
    pub(crate) sub: NumTy,
    // Whether or not something is global in other modules depends on whether or not it is a "local
    // global", i.e. a global that is only referenced from main.
    global: bool,
}

impl Ident {
    fn new_global(low: NumTy) -> Ident {
        Ident {
            low,
            sub: 0,
            global: true,
        }
    }
    fn unused() -> Ident {
        Ident::new_global(0)
    }
    fn new_local(low: NumTy) -> Ident {
        Ident {
            low,
            sub: 0,
            global: false,
        }
    }

    pub(crate) fn is_global(&self, local_globals: &HashSet<NumTy>) -> bool {
        self.global && local_globals.get(&self.low).is_none()
    }

    // used in some test programs to normalize Idents by replacing their subscript with 0
    pub(crate) fn _base(&self) -> Ident {
        Ident {
            low: self.low,
            sub: 0,
            global: self.global,
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub enum Escaper {
    CSV,
    TSV,
    #[default]
    Identity,
}

#[derive(Debug, Clone)]
pub(crate) enum PrimVal<'a> {
    Var(Ident),
    ILit(i64),
    FLit(f64),
    StrLit(&'a [u8]),
}

#[derive(Debug, Clone)]
pub(crate) enum PrimExpr<'a> {
    Val(PrimVal<'a>),
    Phi(SmallVec<(NodeIx /* pred */, Ident)>),
    CallBuiltin(builtins::Function, SmallVec<PrimVal<'a>>),
    Sprintf(PrimVal<'a>, SmallVec<PrimVal<'a>>),
    CallUDF(NumTy, SmallVec<PrimVal<'a>>),
    Index(PrimVal<'a>, PrimVal<'a>),

    // For iterating over vectors.
    IterBegin(PrimVal<'a>),
    HasNext(PrimVal<'a>),
    Next(PrimVal<'a>),
    LoadBuiltin(builtins::Variable),
}

#[derive(Debug)]
pub(crate) enum PrimStmt<'a> {
    AsgnIndex(
        Ident, /* map */
        PrimVal<'a>, /* index */
        PrimExpr<'a>, /* assign to */
    ),
    AsgnVar(Ident /* var */, PrimExpr<'a>),
    SetBuiltin(builtins::Variable, PrimExpr<'a>),
    Return(PrimVal<'a>),
    IterDrop(PrimVal<'a>),

    // Printf is its own node because it is easier to handle varargs explicitly rather than to
    // refactor the whole `builtins` module to support them.
    Printf(
        /*spec*/ PrimVal<'a>,
        /* args */ SmallVec<PrimVal<'a>>,
        /* output */ Option<(PrimVal<'a>, FileSpec)>,
    ),
    PrintAll(
        /* args */ SmallVec<PrimVal<'a>>,
        /* output */ Option<(PrimVal<'a>, FileSpec)>,
    ),
}

// only add constraints when doing an AsgnVar. Because these things are "shallow" it works.
// Maybe also keep an auxiliary map from Var => node in graph, also allow Key(map_ident)
// Val(map_ident).
//
// Build up network. then solve. then use that to insert conversions when producing bytecode.

impl<'a> PrimVal<'a> {
    fn replace(&mut self, mut update: impl FnMut(Ident) -> Ident) {
        if let PrimVal::Var(ident) = self {
            *ident = update(*ident)
        }
    }
}

impl<'a> PrimExpr<'a> {
    fn replace(&mut self, mut update: impl FnMut(Ident) -> Ident) {
        use PrimExpr::*;
        match self {
            Val(v) => v.replace(update),
            Phi(_) => {}
            Sprintf(fmt, args) => {
                fmt.replace(&mut update);
                for a in args.iter_mut() {
                    a.replace(&mut update)
                }
            }
            CallBuiltin(_, args) | CallUDF(_, args) => {
                for a in args.iter_mut() {
                    a.replace(&mut update)
                }
            }
            Index(v1, v2) => {
                v1.replace(&mut update);
                v2.replace(update);
            }
            IterBegin(v) => v.replace(update),
            HasNext(v) => v.replace(update),
            Next(v) => v.replace(update),
            LoadBuiltin(_) => {}
        }
    }
}

impl<'a> PrimStmt<'a> {
    // TODO: remove pub(crate) once we consolidate the cfg modules
    pub(crate) fn replace(&mut self, mut update: impl FnMut(Ident) -> Ident) {
        use PrimStmt::*;
        match self {
            AsgnIndex(ident, v, exp) => {
                *ident = update(*ident);
                v.replace(&mut update);
                exp.replace(update);
            }
            // We handle assignments separately. Note that this is not needed for index
            // expressions, because assignments to m[k] are *uses* of m; it doesn't assign to it.
            AsgnVar(_, e) => e.replace(update),
            SetBuiltin(_, e) => e.replace(update),
            PrintAll(specs, output) => {
                for s in specs.iter_mut() {
                    s.replace(&mut update);
                }
                if let Some((out, _)) = output {
                    out.replace(update);
                }
            }
            Printf(fmt, specs, output) => {
                fmt.replace(&mut update);
                for s in specs.iter_mut() {
                    s.replace(&mut update);
                }
                if let Some((out, _)) = output {
                    out.replace(update);
                }
            }
            IterDrop(v) | Return(v) => v.replace(update),
        }
    }

    /// Calls `f` on the variables read by this statement (those `replace` would update).
    fn visit_uses(&self, mut f: impl FnMut(Ident)) {
        fn val(v: &PrimVal, f: &mut impl FnMut(Ident)) {
            if let PrimVal::Var(id) = v {
                f(*id)
            }
        }
        fn expr(e: &PrimExpr, f: &mut impl FnMut(Ident)) {
            use PrimExpr::*;
            match e {
                Val(v) | IterBegin(v) | HasNext(v) | Next(v) => val(v, f),
                Phi(_) | LoadBuiltin(_) => {}
                Sprintf(fmt, args) => {
                    val(fmt, f);
                    args.iter().for_each(|a| val(a, f));
                }
                CallBuiltin(_, args) | CallUDF(_, args) => args.iter().for_each(|a| val(a, f)),
                Index(v1, v2) => {
                    val(v1, f);
                    val(v2, f);
                }
            }
        }
        use PrimStmt::*;
        match self {
            AsgnIndex(ident, v, e) => {
                f(*ident);
                val(v, &mut f);
                expr(e, &mut f);
            }
            AsgnVar(_, e) | SetBuiltin(_, e) => expr(e, &mut f),
            PrintAll(specs, output) => {
                specs.iter().for_each(|s| val(s, &mut f));
                if let Some((out, _)) = output {
                    val(out, &mut f);
                }
            }
            Printf(fmt, specs, output) => {
                val(fmt, &mut f);
                specs.iter().for_each(|s| val(s, &mut f));
                if let Some((out, _)) = output {
                    val(out, &mut f);
                }
            }
            IterDrop(v) | Return(v) => val(v, &mut f),
        }
    }
}

/// The name of a called function, as written in the program.
fn call_name<I: fmt::Display>(f: &Either<I, builtins::Function>) -> String {
    match f {
        Either::Left(name) => name.to_string(),
        Either::Right(bi) => bi.awk_name().map_or_else(|| bi.to_string(), String::from),
    }
}

/// A short description of an expression that cannot be assigned to, for error messages.
fn describe_non_lhs<I: fmt::Display>(e: &ast::Expr<I>) -> String {
    use ast::Expr::*;
    match e {
        Call(f, _) => format!("`{}(...)`", call_name(f)),
        StrLit(_) => "a string constant".into(),
        ILit(_) | FLit(_) => "a number".into(),
        PatLit(_) => "a regex".into(),
        Getline { .. } | ReadStdin => "a `getline` expression".into(),
        _ => "an expression".into(),
    }
}

fn valid_lhs<I>(e: &ast::Expr<I>) -> bool {
    use ast::Expr::*;
    matches!(e, Index(..) | Var(..) | Unop(ast::Unop::Column, _))
}

#[derive(Debug)]
pub(crate) struct ProgramContext<'a, I> {
    shared: GlobalContext<I>,
    // Functions "know" which Option<Ident> maps to which offset in this
    // table at construction time (in the func_table passed to View).
    pub funcs: Vec<Function<'a, I>>,
    main_offset: Stage<usize>,
    // Reject strings that may contain user input being passed to a subshell (taint analysis).
    // Off by default, as in gawk.
    pub taint_check: bool,
    // Lower certain regular expression instructions to direct invocations of a given pattern,
    // rather than dynamic lookups
    pub fold_regex_constants: bool,
    // Thread through information regarding header columns used.
    pub parse_header: bool,
}

impl<'a, I> ProgramContext<'a, I> {
    pub fn main_stage(&self) -> &Stage<usize> {
        &self.main_offset
    }
    pub fn main_offsets(&self) -> impl Iterator<Item=usize> + '_ + use<'_, I> {
        self.main_offset.iter().cloned()
    }
}

impl<'a> ProgramContext<'a, &'a str> {
    pub(crate) fn dbg_print(&self, w: &mut impl io::Write) -> io::Result<()> {
        for f in self.funcs.iter() {
            write!(w, "function {}={}(", f.name, f.ident)?;
            for (i, a) in f.args.iter().enumerate() {
                use crate::display::Wrap;
                write!(w, "{}={}", a.name, Wrap(a.id))?;
                if i != f.args.len() - 1 {
                    write!(w, ", ")?;
                }
            }
            writeln!(w, ") {{")?;
            dbg_print(&f.cfg, w)?;
            writeln!(w, "\n}}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub enum SepAssign<'a> {
    Potential {
        field_sep: Option<&'a [u8]>,
        record_sep: Option<&'a [u8]>,
    },
    Unsure,
}

impl<'a, I> ProgramContext<'a, I>
    where
        builtins::Variable: TryFrom<I>,
        builtins::Function: TryFrom<I>,
        I: IsSprintf
        + FromStatic
        + Hash
        + Eq
        + Clone
        + Default
        + std::fmt::Display
        + std::fmt::Debug
        + From<&'a str>,
{
    pub(crate) fn local_globals(&mut self) -> HashSet<NumTy> {
        std::mem::take(&mut self.shared.local_globals)
    }
    pub(crate) fn local_globals_ref(&self) -> &HashSet<NumTy> {
        &self.shared.local_globals
    }

    /// Names of the global variables, keyed by the `low` field of their identifiers; used in error
    /// messages.
    /// Where variables and builtin calls are used in the program text, for error messages.
    pub(crate) fn use_locs(&self) -> &UseLocs {
        &self.shared.uses
    }

    pub(crate) fn global_names(&self) -> HashMap<NumTy, I> {
        self.shared
            .hm
            .iter()
            .filter(|(_, id)| id.global)
            .map(|(name, id)| (id.low, name.clone()))
            .collect()
    }

    // We want to optimize scripts that never override FS after the start of the program. We do
    // this by collecting any builtin variable assignments (as well as getline and UDF calls)
    // across all functions and providing a guess of what the FS and RS variables will be for the
    // entire program, when it is safe to assume that no getline call could observe a different
    // value.
    //
    // TODO This is all a bit crude, but we might have to build up full def-use chains to do
    // something more principled. It's worth looking at a more robust approach if there are scripts
    // that we would prefer triggered this optimizations but did not.
    fn begin_offset(&self) -> Option<usize> {
        match self.main_offset {
            Stage::Main(x) => Some(x),
            Stage::Par { begin, .. } => begin,
        }
    }
    pub fn analyze_sep_assignments(&self) -> SepAssign<'a> {
        let mut field_sep = None;
        let mut record_sep = None;
        let mut has_getline = false;
        for (i, f) in self.funcs.iter().enumerate() {
            if Some(i) == self.begin_offset() {
                for (bi, sep) in [
                    (builtins::Variable::FS, &mut field_sep),
                    (builtins::Variable::RS, &mut record_sep),
                ]
                    .iter_mut()
                {
                    if let Some(v) = f.vars.get(&Some(*bi)) {
                        let num_assigns = v.len();
                        if num_assigns == 0 {
                            continue;
                        }
                        if num_assigns > 1 {
                            return SepAssign::Unsure;
                        }
                        let (bb, v) = v[0];
                        if bb != 0 {
                            return SepAssign::Unsure;
                        }
                        // FS/RS assigned to a non-string-literal value.
                        if v.is_none() {
                            return SepAssign::Unsure;
                        }
                        **sep = v;
                    }
                }
                let check_getline = |v: &Vec<(usize, Option<&[u8]>)>| {
                    if !v.is_empty() && v.iter().any(|(bb, _)| *bb == 0) {
                        Some(())
                    } else {
                        None
                    }
                };
                if f.vars.get(&None).and_then(check_getline).is_some() {
                    has_getline = true;
                }
            } else {
                for bi in [builtins::Variable::FS, builtins::Variable::RS].iter() {
                    if f.vars.get(&Some(*bi)).is_some() {
                        return SepAssign::Unsure;
                    }
                }
            }
        }
        // We called getline() _and_ assigned to FS/RS in the begin block; let's bail out just to
        // be safe.
        if has_getline && (field_sep.is_some() || record_sep.is_some()) {
            return SepAssign::Unsure;
        }
        SepAssign::Potential {
            field_sep,
            record_sep,
        }
    }

    // for debugging: get a mapping from the raw identifiers to the synthetic ones.
    pub(crate) fn _invert_ident(&self) -> HashMap<Ident, I> {
        self.shared
            .hm
            .iter()
            .map(|(k, v)| (*v, k.clone()))
            .collect()
    }

    pub(crate) fn from_prog<'b>(
        arena: &'a arena::Arena,
        p: &ast::Prog<'a, 'b, I>,
        esc: Escaper,
    ) -> Result<Self> {
        // TODO this function is a bit of a slog. It would be nice to break it up.
        let mut shared: GlobalContext<I> = GlobalContext {
            hm: Default::default(),
            local_globals: Default::default(),
            may_rename: Default::default(),
            max: 1, // 0 reserved for assigning to "unused" var for side-effecting operations
            conds: Default::default(),
            esc,
            exit_to_end: matches!(p.stage, Stage::Main(_)) && !p.end.is_empty(),
            uninit: Default::default(),
            uses: Default::default(),
            params: p.decs.iter().map(|d| d.params).collect(),
        };
        let stage = p.desugar_stage(arena);
        shared.uninit = uninit::analyze(arena, &p.decs[..], &stage);
        let mut func_table: HashMap<FunctionName<I>, NumTy> = Default::default();
        let mut funcs: Vec<Function<'a, I>> = Default::default();
        for fundec in p.decs.iter() {
            if func_table
                .insert(
                    FunctionName::Named(fundec.name.clone()),
                    funcs.len() as NumTy,
                )
                .is_some()
            {
                return syntax_err!("function name `{}' previously defined", fundec.name);
            }
            // POSIX and gawk builtins cannot be redefined; zawk's stdlib functions are shadowed by
            // the user-defined function (see `call`).
            if builtins::is_awk_builtin(&fundec.name.to_string()) {
                return syntax_err!("`{}' is a built-in function, it cannot be redefined", fundec.name);
            }
            // Special variables (NR, FS, ...) are always resolved to the global builtin, so using
            // one as a parameter would silently ignore the argument and clobber the global.
            if let Some(arg) = fundec
                .args
                .iter()
                .find(|a| builtins::Variable::try_from((*a).clone()).is_ok())
            {
                return syntax_err!(
                    "function `{}': parameter `{}': cannot use a special variable as a function parameter",
                    fundec.name,
                    arg
                );
            }
            for (i, arg) in fundec.args.iter().enumerate() {
                if let Some(j) = fundec.args[..i].iter().position(|a| a == arg) {
                    return syntax_err!(
                        "function `{}': parameter #{}, `{}', duplicates parameter #{}",
                        fundec.name,
                        i + 1,
                        arg,
                        j + 1
                    );
                }
            }
            // All exit blocks simply return the designated return node. Return statements in the
            // AST will becode assignments to this variable followed by an unconditional jump to
            // this block.
            let ret = shared.fresh_local();
            shared.may_rename.push(ret);
            let mut f = Function::new(
                FunctionName::Named(fundec.name.clone()),
                funcs.len() as NumTy,
            );

            let mut ix = 0;
            f.args = fundec
                .args
                .iter()
                .map(|i| {
                    let name = i.clone();
                    let id = shared.fresh_local();
                    f.args_map.insert(i.clone(), ix);
                    // Args are just like standard local variables --- in fact it's a major
                    // use-case for arguments in AWK.
                    shared.may_rename.push(id);
                    record_ident(&mut f.defsites, &mut f.orig, id, f.entry);
                    ix += 1;
                    Arg { name, id }
                })
                .collect();
            f.ret = ret;
            funcs.push(f);
        }
        // A parameter named after a function shadows it in the body; gawk rejects that.
        for fundec in p.decs.iter() {
            if let Some(arg) = fundec
                .args
                .iter()
                .find(|a| func_table.contains_key(&FunctionName::Named((*a).clone())))
            {
                return syntax_err!(
                    "function `{}': parameter `{}': cannot use a function name as a parameter name",
                    fundec.name,
                    arg
                );
            }
        }
        // Now that we have all the functions in place, it's time to fill them up and convert them
        // to SSA.
        macro_rules! fill {
            ($stmt: expr, $name:expr) => {
                if let Some(s) = $stmt {
                    let offset = funcs.len();
                    let mut func = Function::new($name, offset as NumTy);
                    View {
                        ctx: &mut shared,
                        f: &mut func,
                        func_table: &func_table,
                        parse_header: p.parse_header,
                        locs: &p.locs,
                        typed_vars: Default::default(),
                    }
                    .fill(s)?;
                    func_table.insert($name, offset as NumTy);
                    funcs.push(func);
                    Some(offset)
                } else {
                    None
                }
            };
        }

        for fundec in p.decs.iter() {
            let f = *func_table
                .get_mut(&FunctionName::Named(fundec.name.clone()))
                .unwrap();
            View {
                ctx: &mut shared,
                f: funcs.get_mut(f as usize).unwrap(),
                func_table: &func_table,
                parse_header: p.parse_header,
                locs: &p.locs,
                typed_vars: Default::default(),
            }
                .fill(fundec.body)?;
        }

        // Bind the main function
        let main_offset = match stage {
            Stage::Main(main_stmt) => {
                Stage::Main(fill!(Some(main_stmt), FunctionName::MainLoop).unwrap())
            }
            Stage::Par {
                begin: None,
                main_loop: None,
                end: None,
            } => Stage::Main(
                fill!(Some(&Stmt::Block(arena.new_vec())), FunctionName::Begin).unwrap(),
            ),
            Stage::Par {
                begin,
                main_loop,
                end,
            } => {
                // Need to fill begin and end before main_loop to ensure that variables accessed in
                // those two as well as main are marked as global.
                let begin = fill!(begin, FunctionName::Begin);
                let end = fill!(end, FunctionName::End);
                let main_loop = fill!(main_loop, FunctionName::MainLoop);
                Stage::Par {
                    begin,
                    main_loop,
                    end,
                }
            }
        };

        // A global variable (or array) named after a function: `foo = 1` next to `function foo`,
        // or a call with a space before `(`, which is a concatenation with the variable `foo`.
        for fundec in p.decs.iter() {
            if let Some(id) = shared.hm.get(&fundec.name) {
                let e = err_raw!(
                    "function `{}' is used as a variable or an array (a call must not have a space before `(')",
                    fundec.name
                );
                return Err(match shared.uses.any.get(&id.low) {
                    Some(loc) => e.with_loc(*loc),
                    None => e,
                });
            }
        }

        Ok(ProgramContext {
            shared,
            funcs,
            main_offset,
            taint_check: false,
            fold_regex_constants: false,
            parse_header: p.parse_header,
        })
    }
}

struct View<'a, 'b, I> {
    ctx: &'a mut GlobalContext<I>,
    f: &'a mut Function<'b, I>,
    func_table: &'a HashMap<FunctionName<I>, NumTy>,
    parse_header: bool,
    // Where calls and variable references start in the program text.
    locs: &'a ast::ExprLocs,
    // Variable references (keyed by `ast::expr_key`) that are not used as scalars: arrays that
    // are indexed or iterated over, and arguments of calls (which may be arrays).
    typed_vars: HashSet<usize>,
}

/// The first location where each variable (keyed by the `low` field of its identifier) is used,
/// and where each builtin function is called: type errors only know which variable or builtin
/// call they are about, these give them a location.
#[derive(Debug, Default)]
pub(crate) struct UseLocs {
    /// Any use of the variable.
    pub any: HashMap<NumTy, Loc>,
    /// A use as a scalar: read as a value, or assigned to.
    pub scalar: HashMap<NumTy, Loc>,
    /// A use as an array: indexed, or iterated over.
    pub array: HashMap<NumTy, Loc>,
    /// A call of a builtin, keyed by its name (`builtins::Function::awk_name`).
    pub builtin_calls: HashMap<&'static str, Loc>,
}

#[derive(Debug)]
struct GlobalContext<I> {
    // Map the identifiers from the AST to this IR's Idents.
    hm: HashMap<I, Ident>,
    // Global identifiers to rewrite global => local. We only store the `low` field of the
    // identifier.
    local_globals: HashSet<NumTy>,

    // Many identifiers are generated and assigned to only once by construction, so we do not add
    // them to the work list for renaming. All named identifiers are added, as well as the ones
    // created during evaluation of conditional expression (?:, and, or). Doing this saves us not
    // only time, but also extra phi nodes in the IR, which eventually lead to more assignments
    // than are required.
    // TODO: make may_rename per-function for local variables.
    may_rename: Vec<Ident>,
    max: NumTy,
    conds: HashMap<usize, Ident>,
    esc: Escaper,
    // Whether `exit` outside of the END block has to run the END block first (POSIX): true for
    // serial programs with an END block.
    exit_to_end: bool,
    // Numeric variables and arrays whose uninitialized value may be read as a string.
    uninit: uninit::Uninit<I>,
    uses: UseLocs,
    // The number of declared parameters of each user-defined function (indexed by its id): the
    // `local` variables of its body follow them in `Function::args`.
    params: Vec<usize>,
}

impl<I> GlobalContext<I> {
    fn fresh(&mut self) -> Ident {
        let res = self.max;
        self.max += 1;
        Ident::new_global(res)
    }

    fn fresh_local(&mut self) -> Ident {
        let res = self.max;
        self.max += 1;
        Ident::new_local(res)
    }
}

#[derive(Debug)]
pub(crate) struct Arg<I> {
    pub name: I,
    pub id: Ident,
}

// Variable assignments, used to extract fast paths for splitting.
// None indicates a call to `getline`.
type VarAssigns<'a> =
HashMap<Option<builtins::Variable>, Vec<(/* basic block */ usize, Option<&'a [u8]>)>>;

#[derive(Debug)]
pub(crate) struct Function<'a, I> {
    pub name: FunctionName<I>,
    pub ident: NumTy,
    // args_map maps from ast-level ident to an index into args.
    args_map: HashMap<I, NumTy>,
    pub args: SmallVec<Arg<I>>,
    ret: Ident,
    pub cfg: Cfg<'a>,

    defsites: HashMap<Ident, HashSet<NodeIx>>,
    orig: HashMap<NodeIx, HashSet<Ident>>,
    pub entry: NodeIx,
    // We enforce that a single basic block has a return statement. This is to ensure that type
    // inference infers the same type for each return site.
    pub exit: NodeIx,
    // Stack of the entry and exit nodes for the loops (and switch statements, marked by the last
    // field) within which the current statement is nested. `break` leaves the innermost entry,
    // `continue` restarts the innermost loop.
    loop_ctx: SmallVec<(NodeIx, NodeIx, Option<NodeIx>, /*is_switch*/ bool)>,
    // Header node for the toplevel "pattern matching" loop of the AWK program. This is used to
    // implement the nonlocal continue of the `next` and `nextfile` statements.
    //
    // NB: We only support doing this from main.
    toplevel_header: Option<NodeIx>,

    // Entry node of the END block, the target of `exit` from BEGIN or the main loop.
    end_entry: Option<NodeIx>,
    // Whether statements of the END block are being converted.
    in_end: bool,

    vars: VarAssigns<'a>,

    // Dominance information about `cfg`.
    dt: dom::Tree,
    df: dom::Frontier,
}

impl<'a, I> Function<'a, I> {
    fn new(name: FunctionName<I>, ident: NumTy) -> Function<'a, I> {
        let mut cfg = Cfg::default();
        let entry = cfg.add_node(Default::default());
        let exit = cfg.add_node(Default::default());
        Function {
            name,
            ident,
            args: Default::default(),
            args_map: Default::default(),
            ret: Ident::unused(),
            cfg,
            defsites: Default::default(),
            orig: Default::default(),
            entry,
            exit,
            loop_ctx: Default::default(),
            toplevel_header: None,
            end_entry: None,
            in_end: false,
            vars: Default::default(),
            dt: Default::default(),
            df: Default::default(),
        }
    }
}

pub(crate) fn is_unused(i: Ident) -> bool {
    i.low == 0
}

/// Push the operands of a chain of concatenations `e` (or `e` itself, if it is not one) onto
/// `out`, left to right.
fn flatten_concat<'a, 'c, 'b, I>(
    e: &'a Expr<'c, 'b, I>,
    out: &mut SmallVec<&'a Expr<'c, 'b, I>>,
) {
    match e {
        Expr::Binop(ast::Binop::Concat, l, r) => {
            flatten_concat(l, out);
            flatten_concat(r, out);
        }
        _ => out.push(e),
    }
}

fn record_ident(
    defsites: &mut HashMap<Ident, HashSet<NodeIx>>,
    orig: &mut HashMap<NodeIx, HashSet<Ident>>,
    id: Ident,
    blk: NodeIx,
) {
    defsites.entry(id).or_insert(HashSet::default()).insert(blk);

    orig.entry(blk).or_insert(HashSet::default()).insert(id);
}

impl<'a, 'b, I: Hash + Eq + Clone + Default + std::fmt::Display + std::fmt::Debug> View<'a, 'b, I>
    where
        builtins::Variable: TryFrom<I>,
        builtins::Function: TryFrom<I>,
        I: IsSprintf + FromStatic,
{
    fn fill<'c>(&mut self, stmt: &'c Stmt<'c, 'b, I>) -> Result<()> {
        if self.ctx.exit_to_end && self.f.name.is_main() {
            // Initialize the hidden exit variables, so they are always typed as integers.
            let (exiting, code) = self.exit_vars();
            for id in [exiting, code] {
                self.add_stmt(self.f.entry, PrimStmt::AsgnVar(id, PrimExpr::Val(PrimVal::ILit(0))))?;
            }
        }
        // Add a Cfg corresponding to `stmt`
        let next = self.convert_stmt(stmt, self.f.entry)?;
        // Falling off the end of a function returns no value.
        self.mark_returned(false, next)?;
        // Insert edges to the exit nodes if where they do not exist
        self.finish()?;
        // SSA Conversion:
        // 1. Compute the dominator tree and dominance frontiers
        let (dt, df) = {
            let di = dom::DomInfo::new(&self.f.cfg, self.f.entry);
            (di.dom_tree(), di.dom_frontier())
        };
        self.f.dt = dt;
        self.f.df = df;
        // 2. Insert phi nodes where appropriate
        self.insert_phis();
        // 3. Rename variables
        self.rename(self.f.entry);
        Ok(())
    }

    // We want to make sure there is a single exit node. This method adds an unconditional branch
    // to the end of each basic block without one already to the exit node.
    fn finish(&mut self) -> Result<()> {
        for bb in self.f.cfg.node_indices() {
            if bb == self.f.exit {
                continue;
            }
            let mut found = false;
            let mut walker = self.f.cfg.neighbors(bb).detach();
            while let Some((e, _)) = walker.next(&self.f.cfg) {
                if let Transition(None) = self.f.cfg.edge_weight(e).unwrap() {
                    found = true;
                }
            }
            if !found {
                self.f.cfg.add_edge(bb, self.f.exit, Transition::null());
            }
        }
        // Add a return statement to the exit node.
        self.add_stmt(self.f.exit, PrimStmt::Return(PrimVal::Var(self.f.ret)))
    }

    fn standalone_expr<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        in_cond: bool,
    ) -> Result<(NodeIx /*start*/, NodeIx /*end*/, PrimExpr<'b>)> {
        let start = self.f.cfg.add_node(Default::default());
        let (end, res) = self.convert_expr_inner(expr, start, in_cond)?;
        Ok((start, end, res))
    }

    pub fn standalone_block<'c>(
        &mut self,
        stmt: &'c Stmt<'c, 'b, I>,
    ) -> Result<(NodeIx /*start*/, NodeIx /*end*/)> {
        let start = self.f.cfg.add_node(Default::default());
        let end = self.convert_stmt(stmt, start)?;
        Ok((start, end))
    }

    fn get_cond(&mut self, cond: usize) -> Ident {
        if let Some(i) = self.ctx.conds.get(&cond) {
            return *i;
        }
        let i = self.fresh_local();
        self.ctx.conds.insert(cond, i);
        self.ctx.may_rename.push(i);
        i
    }

    fn convert_stmt<'c>(
        &mut self,
        stmt: &'c Stmt<'c, 'b, I>,
        mut current_open: NodeIx,
    ) -> Result<NodeIx> /*next open */ {
        use Stmt::*;
        Ok(match stmt {
            StartCond(cond) => {
                self.set_cond(current_open, *cond, 1)?;
                current_open
            }
            EndCond(cond) => {
                self.set_cond(current_open, *cond, 0)?;
                current_open
            }
            LastCond(cond) => {
                self.set_cond(current_open, *cond, 2)?;
                current_open
            }
            &Expr(&ast::Expr::Call(Either::Right(builtins::Function::Exit), args)) if args.len() <= 1 => {
                self.do_exit(args.first().copied(), current_open)?
            }
            Expr(e) => {
                // We need to assign to unused here, otherwise we could generate the expression but
                // then drop it on the floor.
                let (next, pe) = self.convert_expr(e, current_open)?;
                // The value of an array assignment statement (`a[k] = v`) is a re-read of the
                // element that was just stored. Dropping it avoids an extra Lookup plus a
                // conversion to Int, which would also flatten a Concat rope on every append.
                let is_index_assign = matches!(
                    e,
                    ast::Expr::Assign(ast::Expr::Index(..), _)
                        | ast::Expr::AssignOp(ast::Expr::Index(..), _, _)
                ) && matches!(pe, PrimExpr::Index(..));
                if !is_index_assign {
                    self.add_stmt(next, PrimStmt::AsgnVar(Ident::unused(), pe))?;
                }
                next
            }
            Block(stmts) => {
                for s in stmts {
                    current_open = self.convert_stmt(s, current_open)?;
                }
                current_open
            }
            Printf(fmt, args, out) => {
                let (mut current_open, fmt_v) = self.convert_val(fmt, current_open)?;
                let fmt_v = self.snapshot_before(fmt_v, args, current_open)?;
                let mut arg_vs = SmallVec::with_capacity(args.len());
                for (i, a) in args.iter().enumerate() {
                    let (next, arg_v) = self.convert_val(a, current_open)?;
                    let arg_v = self.snapshot_before(arg_v, &args[i + 1..], next)?;
                    arg_vs.push(arg_v);
                    current_open = next;
                }
                let out_v = if let Some((out, spec)) = out {
                    let (next, out_v) = self.convert_val(out, current_open)?;
                    current_open = next;
                    Some((out_v, *spec))
                } else {
                    None
                };
                self.add_stmt(current_open, PrimStmt::Printf(fmt_v, arg_vs, out_v))?;
                current_open
            }
            Print(vs, out) => {
                let ors = {
                    let ors = self.fresh_local();
                    self.add_stmt(
                        current_open,
                        PrimStmt::AsgnVar(ors, PrimExpr::LoadBuiltin(builtins::Variable::ORS)),
                    )?;
                    PrimVal::Var(ors)
                };
                let (next, out) = if let Some((o, spec)) = out {
                    let (next, e) = self.convert_val(o, current_open)?;
                    (next, Some((e, *spec)))
                } else {
                    (current_open, None)
                };
                current_open = next;

                // Why a macro? breaking this out into methods too easily runs afoul of aliasing
                // rules, a previous version here had to split out several local variables into
                // parameters of outer functions; it was a lot more code.
                if vs.is_empty() {
                    let tmp = self.fresh_local();
                    self.add_stmt(
                        current_open,
                        PrimStmt::AsgnVar(
                            tmp,
                            PrimExpr::CallBuiltin(
                                builtins::Function::Unop(Unop::Column),
                                smallvec![PrimVal::ILit(0)],
                            ),
                        ),
                    )?;
                    self.add_stmt(
                        current_open,
                        PrimStmt::PrintAll(smallvec![PrimVal::Var(tmp), ors], out.clone()),
                    )?;
                    return Ok(current_open);
                }
                let fs = if vs.len() > 1 {
                    let fs = self.fresh_local();
                    self.add_stmt(
                        current_open,
                        PrimStmt::AsgnVar(fs, PrimExpr::LoadBuiltin(builtins::Variable::OFS)),
                    )?;
                    PrimVal::Var(fs)
                } else {
                    PrimVal::Var(Ident::unused())
                };
                // A concatenation is printed piece by piece (`print $1 ":" $2` writes three
                // strings) rather than building the concatenated string first. Each piece is
                // still converted with CONVFMT, as in the concatenation. Escaped output (CSV/TSV)
                // escapes whole arguments, so it keeps them intact.
                let mut leaves: SmallVec<&ast::Expr<'_, 'b, I>> = SmallVec::new();
                // (index of the arg's first leaf in `leaves`, whether it was split)
                let mut arg_leaves = SmallVec::<(usize, bool)>::new();
                for v in vs.iter() {
                    let start = leaves.len();
                    if matches!(self.ctx.esc, Escaper::Identity) && self.uninit_flag(v).is_none()
                    {
                        flatten_concat(v, &mut leaves);
                    } else {
                        leaves.push(*v);
                    }
                    arg_leaves.push((start, leaves.len() - start > 1));
                }
                let mut print_args = SmallVec::with_capacity(leaves.len() + vs.len());
                for (i, &(start, split)) in arg_leaves.iter().enumerate() {
                    let end = arg_leaves.get(i + 1).map_or(leaves.len(), |(s, _)| *s);
                    for j in start..end {
                        let leaf = leaves[j];
                        let (next, mut to_print) = if split {
                            let (next, v) =
                                self.convert_str_operand(leaf, current_open, /*ofmt=*/ false)?;
                            if matches!(leaf, ast::Expr::StrLit(_) | ast::Expr::Unop(Unop::Column, _)) {
                                (next, v)
                            } else {
                                // Convert numbers with CONVFMT (`v ""` is `v` as a string).
                                let s = PrimExpr::CallBuiltin(
                                    builtins::Function::Binop(ast::Binop::Concat),
                                    smallvec![v, PrimVal::StrLit(b"")],
                                );
                                (next, self.to_val(s, next)?)
                            }
                        } else {
                            self.convert_str_operand(leaf, current_open, /*ofmt=*/ true)?
                        };
                        to_print = self.snapshot_before(to_print, &leaves[j + 1..], next)?;
                        to_print = self.escape(to_print, next)?;
                        current_open = next;
                        print_args.push(to_print);
                    }
                    if i == vs.len() - 1 {
                        print_args.push(ors.clone());
                    } else {
                        print_args.push(fs.clone());
                    }
                }
                self.add_stmt(current_open, PrimStmt::PrintAll(print_args, out.clone()))?;
                current_open
            }
            If(cond, tcase, fcase) => {
                let tcase = self.standalone_block(tcase)?;
                let fcase = if let Some(fcase) = fcase {
                    Some(self.standalone_block(fcase)?)
                } else {
                    None
                };
                self.do_condition(cond, tcase, fcase, current_open)?
            }
            For(init, cond, update, body) => {
                let init_end = if let Some(i) = init {
                    self.convert_stmt(i, current_open)?
                } else {
                    current_open
                };
                // todo fix this bug with continue bug
                let (h, b_start, _b_end, f) = self.make_loop(
                    body, *update, init_end, /*is_do*/ false, /*is_toplevel*/ false,
                )?;
                let (h_end, cond_val) = if let Some(c) = cond {
                    self.convert_val(c, h)?
                } else {
                    (h, PrimVal::ILit(1))
                };
                self.f
                    .cfg
                    .add_edge(h_end, b_start, Transition::new(cond_val));
                self.f.cfg.add_edge(h_end, f, Transition::null());
                f
            }
            While(is_toplevel, cond, body) => {
                let (h, b_start, _b_end, f) =
                    self.make_loop(body, None, current_open, /*is_do*/ false, *is_toplevel)?;
                let (h_end, cond_val) = self.convert_val(cond, h)?;
                self.f
                    .cfg
                    .add_edge(h_end, b_start, Transition::new(cond_val));
                self.f.cfg.add_edge(h_end, f, Transition::null());
                f
            }
            DoWhile(cond, body) => {
                let (h, b_start, _b_end, f) = self.make_loop(
                    body,
                    None,
                    current_open,
                    /*is_do*/ true,
                    /*is_toplevel*/ false,
                )?;
                let (h_end, cond_val) = self.convert_val(cond, h)?;
                self.f
                    .cfg
                    .add_edge(h_end, b_start, Transition::new(cond_val));
                self.f.cfg.add_edge(h_end, f, Transition::null());
                f
            }
            ForEach(v, array, body) => {
                let v_id = self.get_identifier(v);
                self.record_array_use(array);
                let (next, array_val) = self.convert_val(array, current_open)?;
                current_open = next;
                let array_iter =
                    self.to_val(PrimExpr::IterBegin(array_val.clone()), current_open)?;

                // First, create the loop header, which checks if there are any more elements
                // in the array.
                let cond = PrimExpr::HasNext(array_iter.clone());
                let cond_block = self.f.cfg.add_node(Default::default());
                let cond_v = self.to_val(cond, cond_block)?;
                self.f
                    .cfg
                    .add_edge(current_open, cond_block, Transition::null());

                // Then add a footer to exit the loop from cond. We will add the edge after adding
                // the edge into the loop body, as order matters.
                let footer = self.f.cfg.add_node(Default::default());
                self.add_stmt(footer, PrimStmt::IterDrop(array_iter.clone()))?;

                self.f.loop_ctx.push((cond_block, footer, None, false));

                // Create the body, but start by getting the next element from the iterator and
                // assigning it to `v`
                let update = PrimStmt::AsgnVar(v_id, PrimExpr::Next(array_iter.clone()));
                let body_start = self.f.cfg.add_node(Default::default());
                self.add_stmt(body_start, update)?;
                self.mark_assigned(&ast::Expr::Var(v.clone()), None, body_start)?;
                let body_end = self.convert_stmt(body, body_start)?;
                self.f
                    .cfg
                    .add_edge(cond_block, body_start, Transition::new(cond_v));
                self.f.cfg.add_edge(cond_block, footer, Transition::null());
                self.f
                    .cfg
                    .add_edge(body_end, cond_block, Transition::null());

                self.f.loop_ctx.pop().unwrap();

                footer
            }
            Switch(e, cases) => self.do_switch(e, cases, current_open)?,
            Break => self.do_break_continue(current_open, /*is_break*/ true)?,
            Continue => self.do_break_continue(current_open, /*is_break*/ false)?,
            Next => self.do_next(current_open, /*is_next_file*/ false)?,
            NextFile => self.do_next(current_open, /*is_next_file*/ true)?,
            EndBlock(body) => {
                let entry = self.end_entry();
                self.guarded_else(current_open, entry);
                // `exit` called while running END exits immediately.
                let (exiting, code) = self.exit_vars();
                self.add_stmt(entry, PrimStmt::AsgnVar(exiting, PrimExpr::Val(PrimVal::ILit(0))))?;
                self.f.in_end = true;
                let end = self.convert_stmt(body, entry)?;
                self.f.in_end = false;
                // Exit with the non-zero code of an earlier `exit`, if any.
                let exit_now = self.exit_now_node()?;
                self.f.cfg.add_edge(end, exit_now, Transition::new(PrimVal::Var(code)));
                let next = self.f.cfg.add_node(Default::default());
                self.f.cfg.add_edge(end, next, Transition::null());
                self.seal(end);
                next
            }
            Return(ret) => {
                if !matches!(self.f.name, FunctionName::Named(_)) {
                    return syntax_err!("`return' used outside function context");
                }
                let (current_open, e) = if let Some(ret) = ret {
                    self.convert_expr(ret, current_open)?
                } else {
                    (current_open, PrimExpr::Val(PrimVal::Var(Ident::unused())))
                };
                self.add_stmt(current_open, PrimStmt::AsgnVar(self.f.ret, e))?;
                self.mark_returned(ret.is_some(), current_open)?;
                let exit = self.f.exit;
                self.jump_to(current_open, exit)?
            }
        })
    }

    fn convert_expr<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        self.convert_expr_inner(expr, current_open, /*in_cond=*/ false)
    }
    /// Converts `expr`; errors raised while converting it are located at `expr` when it has a
    /// location in the program text (and no subexpression with one raised them).
    fn convert_expr_inner<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
        in_cond: bool,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        self.record_uses(expr);
        let res = self.convert_expr_unlocated(expr, current_open, in_cond);
        match self.locs.get(&ast::expr_key(expr)) {
            Some(loc) => res.map_err(|e| e.with_loc(*loc)),
            None => res,
        }
    }

    fn loc_of<'c>(&self, e: &'c Expr<'c, 'b, I>) -> Option<Loc> {
        self.locs.get(&ast::expr_key(e)).copied()
    }

    /// Records where the variables that `expr` uses directly are used as scalars or arrays, and
    /// where builtins are called (see `UseLocs`).
    fn record_uses<'c>(&mut self, expr: &'c Expr<'c, 'b, I>) {
        use Expr::*;
        match expr {
            Index(arr, _)
            | Assign(Index(arr, _), _)
            | AssignOp(Index(arr, _), _, _)
            | Inc { x: Index(arr, _), .. } => self.record_array_use(arr),
            Assign(x @ Var(_), _) | AssignOp(x @ Var(_), _, _) | Inc { x: x @ Var(_), .. } => {
                self.record_var_use(x, /*scalar=*/ true)
            }
            Var(_) => {
                let scalar = !self.typed_vars.contains(&ast::expr_key(expr));
                self.record_var_use(expr, scalar)
            }
            Call(f, args) => {
                if let Some(loc) = self.loc_of(expr) {
                    let bi = match f {
                        Either::Left(name) => builtins::Function::try_from(name.clone()).ok(),
                        Either::Right(bi) => Some(*bi),
                    };
                    if let Some(name) = bi.and_then(|bi| bi.awk_name()) {
                        self.ctx.uses.builtin_calls.entry(name).or_insert(loc);
                    }
                }
                for a in args.iter() {
                    if let Var(_) = a {
                        self.typed_vars.insert(ast::expr_key(*a));
                    }
                }
            }
            _ => {}
        }
    }

    /// Records a use of `arr` as an array, if it is a variable.
    fn record_array_use<'c>(&mut self, arr: &'c Expr<'c, 'b, I>) {
        if let Expr::Var(_) = arr {
            self.typed_vars.insert(ast::expr_key(arr));
            if let (Some(loc), Some(id)) = (self.loc_of(arr), self.var_ident(arr)) {
                self.ctx.uses.array.entry(id.low).or_insert(loc);
            }
        }
    }

    /// Records a use of the variable `x`, as a scalar if `scalar`.
    fn record_var_use<'c>(&mut self, x: &'c Expr<'c, 'b, I>, scalar: bool) {
        if let (Some(loc), Some(id)) = (self.loc_of(x), self.var_ident(x)) {
            self.ctx.uses.any.entry(id.low).or_insert(loc);
            if scalar {
                self.ctx.uses.scalar.entry(id.low).or_insert(loc);
            }
        }
    }

    /// The identifier of the variable `x`, unless it is a builtin variable.
    fn var_ident<'c>(&mut self, x: &'c Expr<'c, 'b, I>) -> Option<Ident> {
        match x {
            Expr::Var(name) if builtins::Variable::try_from(name.clone()).is_err() => {
                Some(self.get_identifier(name))
            }
            _ => None,
        }
    }

    fn convert_expr_unlocated<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        mut current_open: NodeIx,
        in_cond: bool,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        use Expr::*;

        // `a[i][j]` parses as indexing the element `a[i]`, which would otherwise be reported as a
        // confusing type error.
        if let Index(Index(..), _)
        | Assign(Index(Index(..), _), _)
        | AssignOp(Index(Index(..), _), _, _)
        | Inc {
            x: Index(Index(..), _),
            ..
        } = expr
        {
            return err!("arrays of arrays such as `a[i][j]` are not supported");
        }

        // This isn't a function because we want the &s to point to the current stack frame.
        macro_rules! to_bool {
            ($e:expr) => {
                &Unop(ast::Unop::Not, &Unop(ast::Unop::Not, $e))
            };
        }
        let res_expr = match expr {
            ILit(n) => PrimExpr::Val(PrimVal::ILit(*n)),
            FLit(n) => PrimExpr::Val(PrimVal::FLit(*n)),
            // A regex literal used as a value means `$0 ~ /re/`; it is kept as a pattern string
            // only where a regex is expected (see `convert_regex_arg`).
            PatLit(_) => {
                use ast::{Binop::*, Expr::*, Unop::*};
                return self
                    .convert_expr(&Binop(IsMatch, &Unop(Column, &ILit(0)), expr), current_open);
            }
            StrLit(s) => PrimExpr::Val(PrimVal::StrLit(s)),
            Cond(cond) => {
                let id = self.get_cond(*cond);
                PrimExpr::Val(PrimVal::Var(id))
            }
            Unop(ast::Unop::Column, Index(fi @ Var(id), ix))
                if matches!(builtins::Variable::try_from(id.clone()), Ok(builtins::Variable::FI)) =>
            {
                return self.column_by_name(fi, ix, current_open);
            }
            Unop(op, e) => {
                let next_cond = in_cond && matches!(op, ast::Unop::Not);
                let (next, v) = self.convert_val_inner(e, current_open, next_cond)?;
                return Ok((
                    next,
                    PrimExpr::CallBuiltin(builtins::Function::Unop(*op), smallvec![v]),
                ));
            }
            Binop(ast::Binop::EQ, Index(arr, ix), StrLit(s))
            | Binop(ast::Binop::EQ, StrLit(s), Index(arr, ix))
                if s.is_empty() =>
            {
                return self.missing_or_empty(arr, ix, current_open);
            }
            Binop(ast::Binop::Concat, Binop(ast::Binop::Concat, ..), _)
            | Binop(ast::Binop::Concat, _, Binop(ast::Binop::Concat, ..)) => {
                return self.convert_concat(expr, current_open);
            }
            Binop(op, e1, e2) => {
                let (str1, str2) = uninit::string_operands(expr);
                let (next, v1) = if str1.is_some() {
                    self.convert_str_operand(e1, current_open, /*ofmt=*/ false)?
                } else {
                    self.convert_val(e1, current_open)?
                };
                let v1 = self.snapshot_before(v1, &[e2], next)?;
                let (next, v2) = if let ast::Binop::IsMatch = op {
                    self.convert_regex_arg(e2, next)?
                } else if str2.is_some() {
                    self.convert_str_operand(e2, next, /*ofmt=*/ false)?
                } else {
                    self.convert_val(e2, next)?
                };
                let v1 = self.widen_sum_operand(*op, v1, &v2, next)?;
                return Ok((
                    next,
                    PrimExpr::CallBuiltin(builtins::Function::Binop(*op), smallvec![v1, v2]),
                ));
            }
            ITE(cond, tcase, fcase) => {
                let res_id = self.fresh_local();
                self.ctx.may_rename.push(res_id);
                let (tstart, tend, te) = self.standalone_expr(tcase, in_cond)?;
                let (fstart, fend, fe) = self.standalone_expr(fcase, in_cond)?;
                self.add_stmt(tend, PrimStmt::AsgnVar(res_id, te))?;
                self.add_stmt(fend, PrimStmt::AsgnVar(res_id, fe))?;
                let next =
                    self.do_condition(cond, (tstart, tend), Some((fstart, fend)), current_open)?;
                return Ok((next, PrimExpr::Val(PrimVal::Var(res_id))));
            }
            And(e1, e2) => {
                return self.convert_expr_inner(
                    &ITE(e1, to_bool!(e2), &ILit(0)),
                    current_open,
                    in_cond,
                );
            }
            Or(e1, e2) => {
                return self.convert_expr_inner(
                    &ITE(e1, &ILit(1), to_bool!(e2)),
                    current_open,
                    in_cond,
                );
            }
            Nullish(l, r) => {
                // res = l; if (!strnum(l)) res = r. The test treats `l` as a strnum, so any
                // string that looks like a zero ("0", "0.0", "+0") is false, as is "".
                //
                // An array element on the left is only read when it is present (as with `k in a`),
                // so `a[k] ?? d` does not create `a[k]`.
                let (fstart, fend, fe) = self.standalone_expr(r, in_cond)?;
                let (next, lv) = if let Index(arr, ix) = l {
                    let (next, arr_v) = self.convert_val_inner(arr, current_open, in_cond)?;
                    let (next, ix_v) = self.convert_index(ix, next, in_cond)?;
                    let present = PrimExpr::CallBuiltin(
                        builtins::Function::Contains,
                        smallvec![arr_v.clone(), ix_v.clone()],
                    );
                    let present = self.to_val(present, next)?;
                    let lookup = self.f.cfg.add_node(Default::default());
                    let lv = self.to_val(PrimExpr::Index(arr_v, ix_v), lookup)?;
                    self.f.cfg.add_edge(next, lookup, Transition::new(present));
                    self.f.cfg.add_edge(next, fstart, Transition::null());
                    (lookup, lv)
                } else {
                    self.convert_val_inner(l, current_open, in_cond)?
                };
                let cond = self.fresh_local();
                self.add_stmt(
                    next,
                    PrimStmt::AsgnVar(
                        cond,
                        PrimExpr::CallBuiltin(builtins::Function::Strnum, smallvec![lv.clone()]),
                    ),
                )?;
                let res = self.fresh_local();
                self.ctx.may_rename.push(res);
                let t_node = self.f.cfg.add_node(Default::default());
                let join = self.f.cfg.add_node(Default::default());
                self.add_stmt(t_node, PrimStmt::AsgnVar(res, PrimExpr::Val(lv.clone())))?;
                self.add_stmt(fend, PrimStmt::AsgnVar(res, fe))?;
                self.f.cfg.add_edge(next, t_node, Transition::new(PrimVal::Var(cond)));
                self.f.cfg.add_edge(next, fstart, Transition::null());
                self.f.cfg.add_edge(t_node, join, Transition::null());
                self.f.cfg.add_edge(fend, join, Transition::null());
                return Ok((join, PrimExpr::Val(PrimVal::Var(res))));
            }
            Var(id) => {
                if let Ok(bi) = builtins::Variable::try_from(id.clone()) {
                    // To maximize compatibility with other scripts, we don't have FI in scope as a
                    // builtin if we are not parsing the header line.
                    if matches!(bi, builtins::Variable::FI) && !self.parse_header {
                        let ident = self.get_identifier(id);
                        PrimExpr::Val(PrimVal::Var(ident))
                    } else {
                        PrimExpr::LoadBuiltin(bi)
                    }
                } else {
                    let ident = self.get_identifier(id);
                    PrimExpr::Val(PrimVal::Var(ident))
                }
            }
            Index(arr, ix) => {
                let (next, arr_v) = self.convert_val_inner(arr, current_open, in_cond)?;
                let (next, ix_v) = self.convert_index(ix, next, in_cond)?;
                return Ok((next, PrimExpr::Index(arr_v, ix_v)));
            }
            Call(fname, args) => return self.call(current_open, fname, args),
            Assign(Index(arr, ix), to) => {
                return self.do_assign_index(
                    arr,
                    ix,
                    |slf, _, _, open| slf.convert_expr(to, open),
                    current_open,
                );
            }

            AssignOp(Index(arr, ix), op, to) => {
                if let ast::Binop::Plus = op {
                    // We don't need in_cond here, it would seem, because there aren't
                    // subexpressions which should be considered patterns.
                    // Adding a non-constant amount is done in floating point, as in
                    // `widen_sum_operand`.
                    let by = &Expr::Call(Either::Right(builtins::Function::ToFloat), &[to]);
                    let by = if matches!(to, ILit(_) | FLit(_)) { to } else { by };
                    return self.convert_expr(
                        &Expr::Call(Either::Right(builtins::Function::IncMap), &[arr, ix, by]),
                        current_open,
                    );
                }
                return self.do_assign_index(
                    arr,
                    ix,
                    |slf, arr_v, ix_v, open| {
                        let (next, to_v) = slf.convert_val(to, open)?;
                        let arr_cell_v = slf.to_val(PrimExpr::Index(arr_v, ix_v.clone()), next)?;
                        let arr_cell_v = slf.widen_sum_operand(*op, arr_cell_v, &to_v, next)?;
                        Ok((
                            next,
                            PrimExpr::CallBuiltin(
                                builtins::Function::Binop(*op),
                                smallvec![arr_cell_v, to_v],
                            ),
                        ))
                    },
                    current_open,
                );
            }
            // A tracked variable assigned an element: it is uninitialized if the element did not
            // exist (see the `uninit` module).
            Assign(x, Index(arr @ Var(a), ix))
                if self.uninit_flag(x).is_some() && !self.f.args_map.contains_key(a) =>
            {
                use builtins::Function;
                let (next, arr_v) = self.convert_val(arr, current_open)?;
                let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
                // Check membership first: the lookup creates the element, as in awk.
                let present =
                    PrimExpr::CallBuiltin(Function::Contains, smallvec![arr_v.clone(), ix_v.clone()]);
                let present = self.to_val(present, next)?;
                let to_e = PrimExpr::Index(arr_v, ix_v);
                let (next, res) = self.do_assign_hint(x, |_| to_e, None, next)?;
                let flag = self.uninit_flag(x).unwrap();
                let flag = self.get_identifier(&flag);
                self.add_stmt(next, PrimStmt::AsgnVar(flag, PrimExpr::Val(present)))?;
                return Ok((next, res));
            }
            Assign(x, to) => {
                let lit = match to {
                    StrLit(s) => Some(*s),
                    _ => None,
                };
                let (next, to_e) = self.convert_expr(to, current_open)?;
                let (next, res) = self.do_assign_hint(x, |_| to_e, lit, next)?;
                self.mark_assigned(x, Some(to), next)?;
                return Ok((next, res));
            }
            AssignOp(x, op, to) => {
                let (next, to_v) = self.convert_val(to, current_open)?;
                // `x` is not a constant: `x += y` is done in floating point unless `y` is one.
                let to_v = self.widen_sum_operand(*op, to_v, &PrimVal::Var(Ident::unused()), next)?;
                let (next, res) = self.do_assign(
                    x,
                    |v| {
                        PrimExpr::CallBuiltin(
                            builtins::Function::Binop(*op),
                            smallvec![v.clone(), to_v],
                        )
                    },
                    next,
                )?;
                self.mark_assigned(x, None, next)?;
                return Ok((next, res));
            }
            Inc { is_inc, is_post, x } => {
                if !valid_lhs(x) {
                    return syntax_err!(
                        "the operand of `{}` must be a variable, an array element or a field",
                        if *is_inc { "++" } else { "--" }
                    );
                };
                if let (true, Index(arr, ix)) = (*is_post, x) {
                    // Evaluate the array and the subscript once: the subscript may have side
                    // effects (`a[i++]++`) or be expensive to compute (`a[$1 % 100]++`).
                    let (next, arr_e) = self.convert_expr(arr, current_open)?;
                    let arr_v = self.to_val(arr_e, next)?;
                    let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
                    // The value of `a[k]++` is the old value as a number: 0 if it was "v".
                    let cell = self.to_val(PrimExpr::Index(arr_v.clone(), ix_v.clone()), next)?;
                    let old = self.fresh_local();
                    self.add_stmt(
                        next,
                        PrimStmt::AsgnVar(
                            old,
                            PrimExpr::CallBuiltin(
                                builtins::Function::Unop(ast::Unop::Pos),
                                smallvec![cell],
                            ),
                        ),
                    )?;
                    let by = PrimVal::ILit(if *is_inc { 1 } else { -1 });
                    self.add_stmt(
                        next,
                        PrimStmt::AsgnVar(
                            Ident::unused(),
                            PrimExpr::CallBuiltin(
                                builtins::Function::IncMap,
                                smallvec![arr_v, ix_v, by],
                            ),
                        ),
                    )?;
                    return Ok((next, PrimExpr::Val(PrimVal::Var(old))));
                }
                let (next, pre) = if *is_post {
                    // The value of `x++` is the old value of x as a number: 0 if x was "v".
                    let (next, e) =
                        self.convert_expr(&ast::Expr::Unop(ast::Unop::Pos, x), current_open)?;
                    let f = self.fresh_local();
                    self.add_stmt(next, PrimStmt::AsgnVar(f, e))?;
                    (next, Some(PrimExpr::Val(PrimVal::Var(f))))
                } else {
                    (current_open, None)
                };
                let (next, post) = self.convert_expr(
                    &ast::Expr::AssignOp(
                        x,
                        ast::Binop::Plus,
                        if *is_inc {
                            &ast::Expr::ILit(1)
                        } else {
                            &ast::Expr::ILit(-1)
                        },
                    ),
                    next,
                )?;
                if *is_post {
                    // Anchor any computation we have to the AST
                    self.add_stmt(next, PrimStmt::AsgnVar(Ident::unused(), post))?;
                    return Ok((next, pre.unwrap()));
                } else {
                    return Ok((next, post));
                }
            }
            ReadStdin => {
                use builtins::Function::{ReadErrStdin, ReadLineStdinFused};
                // break up getline calls to help the FS analysis out
                let next = self.f.cfg.add_node(Default::default());
                self.f.cfg.add_edge(current_open, next, Transition::null());
                current_open = next;
                self.add_stmt(
                    current_open,
                    PrimStmt::AsgnVar(
                        Ident::unused(),
                        PrimExpr::CallBuiltin(ReadLineStdinFused, smallvec![]),
                    ),
                )?;
                return self.convert_expr(
                    &ast::Expr::Call(Either::Right(ReadErrStdin), &[]),
                    current_open,
                );
            }
            Getline {
                from,
                into,
                is_file,
            } => {
                // break up getline calls to help the FS analysis out
                let next = self.f.cfg.add_node(Default::default());
                self.f.cfg.add_edge(current_open, next, Transition::null());
                current_open = next;
                // If we had a `getline` call before assigning to `FS` or `RS` in the BEGIN block,
                // we want to disable any optimizations around field splitting.
                self.f
                    .vars
                    .entry(None)
                    .or_insert_with(Vec::new)
                    .push((current_open.index(), None));
                // Another use of non-structural recursion for desugaring. Here we desugar:
                //   getline var < file
                // to
                //   var = nextline(file)
                //   readerr(file)
                // And we fill in various other pieces of sugar as well. Informally:
                //  getline < file => getline $0 < file
                //  getline var => getline var < stdin
                //  getline => getline $0
                use builtins::Function::{
                    Nextline, NextlineCmd, NextlineStdin, ReadErr, ReadErrCmd, ReadErrStdin,
                };
                let next_line = if *is_file { Nextline } else { NextlineCmd };
                let read_err = if *is_file { ReadErr } else { ReadErrCmd };
                //
                // Only a successful read assigns the target and updates NR (and FNR, when
                // reading the main input); see `getline_result`.
                match (from, into) {
                    // an unadorned `getline` is uses the "fused" stdin construct, which in turn
                    // enables some optimizations. (At the end of the input, it keeps $0.)
                    (None /* stdin */, None /* $0 */) => {
                        return self.getline_result(
                            current_open,
                            None,
                            &ast::Expr::ReadStdin,
                            None,
                            /*main_input=*/ true,
                        );
                    }
                    (from, None /* $0 */) => {
                        return self.convert_expr(
                            &ast::Expr::Getline {
                                from: *from,
                                into: Some(&Unop(ast::Unop::Column, &ast::Expr::ILit(0))),
                                is_file: *is_file,
                            },
                            current_open,
                        );
                    }
                    (Some(from), Some(into)) => {
                        // Reading from a file or command does not update NR or FNR (as in gawk
                        // and onetrue awk).
                        return self.getline_result(
                            current_open,
                            Some(&ast::Expr::Call(Either::Right(next_line), &[from])),
                            &ast::Expr::Call(Either::Right(read_err), &[from]),
                            Some(into),
                            /*main_input=*/ false,
                        );
                    }
                    (None /*stdin*/, Some(into)) => {
                        return self.getline_result(
                            current_open,
                            Some(&ast::Expr::Call(Either::Right(NextlineStdin), &[])),
                            &ast::Expr::Call(Either::Right(ReadErrStdin), &[]),
                            Some(into),
                            /*main_input=*/ true,
                        );
                    }
                };
            }
        };
        Ok((current_open, res_expr))
    }

    /// Lower the end of a getline: `read` (if any) reads the line, `status` is the result of
    /// the getline (1, 0 at end of input, -1 on error). Only if the read succeeded is the line
    /// assigned to `into` and, for the main input, NR and FNR incremented.
    fn getline_result<'c>(
        &mut self,
        current_open: NodeIx,
        read: Option<&'c Expr<'c, 'b, I>>,
        status: &'c Expr<'c, 'b, I>,
        into: Option<&'c Expr<'c, 'b, I>>,
        main_input: bool,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        use ast::Expr::{Assign, Var};
        let line_var = Var(I::from_static("%getline_line"));
        let status_var = Var(I::from_static("%getline_status"));
        let mut next = current_open;
        if let Some(read) = read {
            next = self.convert_expr(&Assign(&line_var, read), next)?.0;
        }
        next = self.convert_expr(&Assign(&status_var, status), next)?.0;
        // Copy the status into a fresh local: another getline in the same expression reuses
        // %getline_status.
        let (next, status_val) = self.convert_expr(&status_var, next)?;
        let res = self.fresh_local();
        self.add_stmt(next, PrimStmt::AsgnVar(res, status_val))?;
        let mut next = next;
        if let Some(into) = into {
            next = self.if_read(next, &Stmt::Expr(&Assign(into, &line_var)))?;
        }
        if main_input {
            for counter in ["NR", "FNR"] {
                let inc = ast::Expr::Inc {
                    is_inc: true,
                    is_post: false,
                    x: &Var(I::from_static(counter)),
                };
                next = self.if_read(next, &Stmt::Expr(&inc))?;
            }
        }
        Ok((next, PrimExpr::Val(PrimVal::Var(res))))
    }

    /// Run `stmt` if the last getline succeeded (its status is positive).
    fn if_read<'c>(&mut self, current_open: NodeIx, stmt: &'c Stmt<'c, 'b, I>) -> Result<NodeIx> {
        let status_var = ast::Expr::Var(I::from_static("%getline_status"));
        let ok = ast::Expr::Binop(ast::Binop::GT, &status_var, &ast::Expr::ILit(0));
        self.convert_stmt(&Stmt::If(&ok, stmt, None), current_open)
    }

    fn guarded_else(&mut self, from: NodeIx, to: NodeIx) {
        if self.f.cfg.node_weight(from).unwrap().sealed {
            return;
        }
        self.f.cfg.add_edge(from, to, Transition::null());
    }

    fn do_sprintf<'c>(
        &mut self,
        args: &'c [&'c Expr<'c, 'b, I>],
        mut current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        if args.is_empty() {
            return err!("sprintf must have at least one argument");
        }
        let (next, fmt) = self.convert_val(args[0], current_open)?;
        let fmt = self.snapshot_before(fmt, &args[1..], next)?;
        current_open = next;
        let mut res = SmallVec::with_capacity(args.len() - 1);
        for (i, a) in args.iter().enumerate().skip(1) {
            let (next, v) = self.convert_val(a, current_open)?;
            let v = self.snapshot_before(v, &args[i + 1..], next)?;
            current_open = next;
            res.push(v);
        }
        Ok((current_open, PrimExpr::Sprintf(fmt, res)))
    }
    fn do_assign<'c>(
        &mut self,
        v: &'c Expr<'c, 'b, I>,
        to: impl FnOnce(&PrimVal<'b>) -> PrimExpr<'b>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        self.do_assign_hint(v, to, None, current_open)
    }

    // The `hint` in this case is `str_lit` which is used to infer potential "fast paths" for field
    // splitting.
    fn do_assign_hint<'c>(
        &mut self,
        v: &'c Expr<'c, 'b, I>,
        to: impl FnOnce(&PrimVal<'b>) -> PrimExpr<'b>,
        str_lit: Option<&'b [u8]>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        use ast::Expr::*;
        match v {
            Var(i) => Ok((
                current_open,
                if let Ok(b) = builtins::Variable::try_from(i.clone()) {
                    // We collect some data on which builtins are assigned to, and if they are
                    // assigned to a string literal. This is used for triggering some fast paths
                    // for field splitting; it is not as precise as the method for inferring used
                    // fields (which consumes the typed IR), but that analysis is also a bit harder
                    // to do soundly in this case.
                    self.f
                        .vars
                        .entry(Some(b))
                        .or_insert_with(Vec::new)
                        .push((current_open.index(), str_lit));
                    let res = PrimExpr::LoadBuiltin(b);
                    let res_v = self.to_val(res.clone(), current_open)?;
                    self.add_stmt(current_open, PrimStmt::SetBuiltin(b, to(&res_v)))?;
                    res
                } else {
                    let ident = self.get_identifier(i);
                    let res_v = PrimVal::Var(ident);
                    self.add_stmt(current_open, PrimStmt::AsgnVar(ident, to(&res_v)))?;
                    PrimExpr::Val(res_v)
                },
            )),
            Unop(ast::Unop::Column, n) => {
                use {ast::Unop::*, builtins::Function};
                let (next, v) = self.convert_val(n, current_open)?;
                let res = PrimExpr::CallBuiltin(Function::Unop(Column), smallvec![v.clone()]);
                let res_v = self.to_val(res.clone(), next)?;
                let to_v = self.to_val(to(&res_v), next)?;
                self.add_stmt(
                    next,
                    PrimStmt::AsgnVar(
                        Ident::unused(),
                        PrimExpr::CallBuiltin(Function::Setcol, smallvec![v, to_v]),
                    ),
                )?;
                Ok((next, res))
            }
            _ => {
                let e = err_raw!(
                    "cannot assign to {}: the left side of `=` must be a variable, an array element or a field; use `==` to compare, or assign the value to a variable first",
                    describe_non_lhs(v)
                );
                Err(match self.loc_of(v) {
                    Some(loc) => e.with_loc(loc),
                    None => e,
                })
            }
        }
    }
    fn do_assign_index<'c>(
        &mut self,
        arr: &'c Expr<'c, 'b, I>,
        ix: &'c Expr<'c, 'b, I>,
        mut to_f: impl FnMut(
            &mut Self,
            PrimVal<'b>,
            PrimVal<'b>,
            NodeIx,
        ) -> Result<(NodeIx, PrimExpr<'b>)>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        let (next, arr_e) = self.convert_expr(arr, current_open)?;

        // Only assign to a new variable if we need to.
        let arr_id = if let PrimExpr::Val(PrimVal::Var(id)) = arr_e {
            id
        } else {
            let arr_id = self.fresh_local();
            self.add_stmt(next, PrimStmt::AsgnVar(arr_id, arr_e))?;
            arr_id
        };

        let arr_v = PrimVal::Var(arr_id);
        let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
        let (next, to_e) = to_f(self, arr_v.clone(), ix_v.clone(), next)?;
        self.add_stmt(
            next,
            PrimStmt::AsgnIndex(arr_id, ix_v.clone(), to_e.clone()),
        )?;
        Ok((next, PrimExpr::Index(arr_v, ix_v)))
    }

    fn do_condition<'c>(
        &mut self,
        cond: &'c Expr<'c, 'b, I>,
        tcase: (NodeIx, NodeIx),
        fcase: Option<(NodeIx, NodeIx)>,
        current_open: NodeIx,
    ) -> Result<NodeIx> {
        let (t_start, t_end) = tcase;
        let (current_open, c_val) =
            self.convert_val_inner(cond, current_open, /*in_cond=*/ true)?;

        let next = self.f.cfg.add_node(Default::default());

        // current_open => t_start if the condition holds
        self.f
            .cfg
            .add_edge(current_open, t_start, Transition::new(c_val));
        // continue to next after the true case is evaluated, unless we are returning.
        self.guarded_else(t_end, next);

        if let Some((f_start, f_end)) = fcase {
            // Set up the same connections as before, this time with a null edge rather
            // than c_val.
            self.f
                .cfg
                .add_edge(current_open, f_start, Transition::null());
            self.f.cfg.add_edge(f_end, next, Transition::null());
        } else {
            // otherwise continue directly from current_open.
            self.guarded_else(current_open, next);
        }
        Ok(next)
    }
    fn convert_val<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        self.convert_val_inner(expr, current_open, /*in_cond=*/ false)
    }

    /// The flag recording whether the tracked variable read by `e` has been assigned, or whether
    /// the tracked function called by `e` returned a value (see the `uninit` module). The flag of
    /// a call is read after the call.
    fn uninit_flag<'c>(&self, e: &'c Expr<'c, 'b, I>) -> Option<I> {
        match e {
            Expr::Var(x) if !self.f.args_map.contains_key(x) => self.ctx.uninit.vars.get(x).cloned(),
            Expr::Call(Either::Left(f), _) => self.ctx.uninit.funcs.get(f).cloned(),
            _ => None,
        }
    }

    /// The flag of the function being converted, if it is tracked (see `uninit_flag`).
    fn ret_flag(&self) -> Option<I> {
        match &self.f.name {
            FunctionName::Named(name) => self.ctx.uninit.funcs.get(name).cloned(),
            _ => None,
        }
    }

    /// Record whether the function being converted returns a value, if it is tracked.
    fn mark_returned(&mut self, returned: bool, current_open: NodeIx) -> Result<()> {
        if let Some(flag) = self.ret_flag() {
            let flag = self.get_identifier(&flag);
            let v = PrimExpr::Val(PrimVal::ILit(returned as i64));
            self.add_stmt(current_open, PrimStmt::AsgnVar(flag, v))?;
        }
        Ok(())
    }

    /// Whether the variable `x` is read as a value somewhere (see the `uninit` module).
    fn scalar_referenced(&self, x: &I) -> bool {
        let func = match &self.f.name {
            FunctionName::Named(name) if self.f.args_map.contains_key(x) => Some(name.clone()),
            _ => None,
        };
        self.ctx.uninit.scalar_refs.contains(&(func, x.clone()))
    }

    /// Whether `e` reads an element of a tracked array.
    fn uninit_map_elem<'c>(&self, e: &'c Expr<'c, 'b, I>) -> bool {
        match e {
            Expr::Index(Expr::Var(a), _) => {
                !self.f.args_map.contains_key(a) && self.ctx.uninit.maps.contains(a)
            }
            _ => false,
        }
    }

    /// After an assignment to `x`: record that a tracked variable has been assigned. A copy of
    /// another tracked variable (`x = y`) copies its flag, as `y` may be uninitialized.
    fn mark_assigned<'c>(
        &mut self,
        x: &'c Expr<'c, 'b, I>,
        rhs: Option<&'c Expr<'c, 'b, I>>,
        current_open: NodeIx,
    ) -> Result<()> {
        let flag = match self.uninit_flag(x) {
            Some(flag) => flag,
            None => return Ok(()),
        };
        let val = match rhs.and_then(|rhs| self.uninit_flag(rhs)) {
            Some(src) => PrimVal::Var(self.get_identifier(&src)),
            // `local x` makes `x` uninitialized again.
            None if matches!(rhs, Some(Expr::Var(v)) if v.to_string() == ast::UNINIT_VAR) => PrimVal::ILit(0),
            None => PrimVal::ILit(1),
        };
        let flag = self.get_identifier(&flag);
        self.add_stmt(current_open, PrimStmt::AsgnVar(flag, PrimExpr::Val(val)))
    }

    /// Converts `e`, which is read as a string. An uninitialized tracked variable, or a missing
    /// element of a tracked array, is "" rather than "0". With `ofmt`, numbers are converted to
    /// strings as print does.
    fn convert_str_operand<'c>(
        &mut self,
        e: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
        ofmt: bool,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        if let Some(flag) = self.uninit_flag(e) {
            let (next, v) = self.convert_val(e, current_open)?;
            let flag = PrimVal::Var(self.get_identifier(&flag));
            return self.str_or_empty(flag, v, ofmt, next);
        }
        if let Expr::Index(arr, ix) = e {
            if self.uninit_map_elem(e) {
                use builtins::Function;
                let (next, arr_v) = self.convert_val(arr, current_open)?;
                let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
                // Check membership first: the lookup creates the element, as in awk.
                let present = PrimExpr::CallBuiltin(
                    Function::Contains,
                    smallvec![arr_v.clone(), ix_v.clone()],
                );
                let present = self.to_val(present, next)?;
                let v = self.to_val(PrimExpr::Index(arr_v, ix_v), next)?;
                return self.str_or_empty(present, v, ofmt, next);
            }
        }
        self.convert_val(e, current_open)
    }

    /// `typeof(e)`, for the cases where the answer depends on the state at runtime (as in gawk):
    /// a tracked variable that has not been assigned yet is "unassigned" (or "untyped" if it is
    /// never read), a missing array element is "untyped" and a field beyond NF is "unassigned".
    fn type_of_dynamic<'c>(
        &mut self,
        e: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<Option<(NodeIx, PrimExpr<'b>)>> {
        use builtins::Function::{self, Binop as Bin, Contains, ToInt, TypeOfVariable};
        let type_of = |v| PrimExpr::CallBuiltin(TypeOfVariable, smallvec![v]);
        let (next, cond, v, otherwise): (_, _, _, &'static [u8]) = match e {
            Expr::Var(x) => {
                let flag = match self.uninit_flag(e) {
                    Some(flag) => flag,
                    None => return Ok(None),
                };
                let otherwise: &'static [u8] =
                    if self.scalar_referenced(x) { b"unassigned" } else { b"untyped" };
                let (next, v) = self.convert_val(e, current_open)?;
                (next, PrimVal::Var(self.get_identifier(&flag)), type_of(v), otherwise)
            }
            Expr::Index(arr, ix) => {
                let (next, arr_v) = self.convert_val(arr, current_open)?;
                let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
                let present =
                    PrimExpr::CallBuiltin(Contains, smallvec![arr_v.clone(), ix_v.clone()]);
                let present = self.to_val(present, next)?;
                // The lookup creates the element, as in gawk.
                let v = self.to_val(PrimExpr::Index(arr_v, ix_v), next)?;
                (next, present, type_of(v), b"untyped")
            }
            Expr::Unop(Unop::Column, n) => {
                let (next, n_v) = self.convert_val(n, current_open)?;
                let n_v = self.to_val(PrimExpr::CallBuiltin(ToInt, smallvec![n_v]), next)?;
                let nf = self.to_val(PrimExpr::LoadBuiltin(builtins::Variable::NF), next)?;
                let in_range = PrimExpr::CallBuiltin(
                    Bin(ast::Binop::LTE),
                    smallvec![n_v.clone(), nf],
                );
                let in_range = self.to_val(in_range, next)?;
                // $0 is "unassigned" until a record is read or it is set; approximate that with
                // `NR == 0 && $0 == ""`, i.e. require `n + NR + length($0) > 0` as well.
                let nr = self.to_val(PrimExpr::LoadBuiltin(builtins::Variable::NR), next)?;
                let n_nr = self.to_val(
                    PrimExpr::CallBuiltin(Bin(ast::Binop::Plus), smallvec![n_v.clone(), nr]),
                    next,
                )?;
                let line = self.to_val(
                    PrimExpr::CallBuiltin(
                        Function::Unop(Unop::Column),
                        smallvec![PrimVal::ILit(0)],
                    ),
                    next,
                )?;
                let len =
                    self.to_val(PrimExpr::CallBuiltin(Function::Length, smallvec![line]), next)?;
                let assigned = self.to_val(
                    PrimExpr::CallBuiltin(Bin(ast::Binop::Plus), smallvec![n_nr, len]),
                    next,
                )?;
                let in_range = self.to_val(
                    PrimExpr::CallBuiltin(Bin(ast::Binop::Mult), smallvec![in_range, assigned]),
                    next,
                )?;
                let v = self.to_val(
                    PrimExpr::CallBuiltin(Function::Unop(Unop::Column), smallvec![n_v]),
                    next,
                )?;
                (next, in_range, type_of(v), b"unassigned")
            }
            _ => return Ok(None),
        };
        let res = self.select(cond, v, PrimExpr::Val(PrimVal::StrLit(otherwise)), next)?;
        Ok(Some((res.0, PrimExpr::Val(res.1))))
    }

    /// `cond ? t : f`.
    fn select(
        &mut self,
        cond: PrimVal<'b>,
        t: PrimExpr<'b>,
        f: PrimExpr<'b>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        let res = self.fresh_local();
        self.ctx.may_rename.push(res);
        let t_node = self.f.cfg.add_node(Default::default());
        let f_node = self.f.cfg.add_node(Default::default());
        let next = self.f.cfg.add_node(Default::default());
        self.add_stmt(t_node, PrimStmt::AsgnVar(res, t))?;
        self.add_stmt(f_node, PrimStmt::AsgnVar(res, f))?;
        self.f.cfg.add_edge(current_open, t_node, Transition::new(cond));
        self.f.cfg.add_edge(current_open, f_node, Transition::null());
        self.f.cfg.add_edge(t_node, next, Transition::null());
        self.f.cfg.add_edge(f_node, next, Transition::null());
        Ok((next, PrimVal::Var(res)))
    }

    /// `$FI[k]`: `(k in FI) ? $(FI[k]) : ""`. A missing column name (a typo, or a missing `-H`)
    /// would otherwise read FI[k] as 0 and silently yield the whole record. The empty string
    /// converts to 0 in numeric contexts, like any missing field. The lookup only happens when
    /// the key is present, so FI is not polluted with misspelled names.
    fn column_by_name<'c>(
        &mut self,
        fi: &'c Expr<'c, 'b, I>,
        ix: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        use builtins::Function;
        let (next, arr_v) = self.convert_val(fi, current_open)?;
        let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
        let present = PrimExpr::CallBuiltin(Function::Contains, smallvec![arr_v.clone(), ix_v.clone()]);
        let present = self.to_val(present, next)?;
        let res = self.fresh_local();
        self.ctx.may_rename.push(res);
        let found = self.f.cfg.add_node(Default::default());
        let missing = self.f.cfg.add_node(Default::default());
        let join = self.f.cfg.add_node(Default::default());
        let col = self.to_val(PrimExpr::Index(arr_v, ix_v), found)?;
        self.add_stmt(
            found,
            PrimStmt::AsgnVar(res, PrimExpr::CallBuiltin(Function::Unop(ast::Unop::Column), smallvec![col])),
        )?;
        self.add_stmt(missing, PrimStmt::AsgnVar(res, PrimExpr::Val(PrimVal::StrLit(b""))))?;
        self.f.cfg.add_edge(next, found, Transition::new(present));
        self.f.cfg.add_edge(next, missing, Transition::null());
        self.f.cfg.add_edge(found, join, Transition::null());
        self.f.cfg.add_edge(missing, join, Transition::null());
        Ok((join, PrimExpr::Val(PrimVal::Var(res))))
    }

    /// `cond ? v : ""`, as a string.
    fn str_or_empty(
        &mut self,
        cond: PrimVal<'b>,
        v: PrimVal<'b>,
        ofmt: bool,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        let res = self.fresh_local();
        self.ctx.may_rename.push(res);
        let set = self.f.cfg.add_node(Default::default());
        let empty = self.f.cfg.add_node(Default::default());
        let next = self.f.cfg.add_node(Default::default());
        let v = if ofmt {
            PrimExpr::CallBuiltin(builtins::Function::OfmtStr, smallvec![v])
        } else {
            PrimExpr::Val(v)
        };
        self.add_stmt(set, PrimStmt::AsgnVar(res, v))?;
        self.add_stmt(empty, PrimStmt::AsgnVar(res, PrimExpr::Val(PrimVal::StrLit(b""))))?;
        self.f.cfg.add_edge(current_open, set, Transition::new(cond));
        self.f.cfg.add_edge(current_open, empty, Transition::null());
        self.f.cfg.add_edge(set, next, Transition::null());
        self.f.cfg.add_edge(empty, next, Transition::null());
        Ok((next, PrimVal::Var(res)))
    }

    /// `a[k] == ""`: true when the element did not exist before the lookup, or its value is
    /// the empty string.
    ///
    /// Map values are statically typed, so a missing element of a numeric map reads as 0 rather
    /// than the uninitialized value; checking membership first keeps this common "was the key
    /// seen?" idiom correct. The lookup still happens, creating the element as in awk.
    fn missing_or_empty<'c>(
        &mut self,
        arr: &'c Expr<'c, 'b, I>,
        ix: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        use builtins::Function;
        let (next, arr_v) = self.convert_val(arr, current_open)?;
        let (next, ix_v) = self.convert_index(ix, next, /*in_cond=*/ false)?;
        let present = PrimExpr::CallBuiltin(Function::Contains, smallvec![arr_v.clone(), ix_v.clone()]);
        let present = self.to_val(present, next)?;
        let val = self.to_val(PrimExpr::Index(arr_v, ix_v), next)?;
        let val_str = PrimExpr::CallBuiltin(
            Function::Binop(ast::Binop::Concat),
            smallvec![val, PrimVal::StrLit(b"")],
        );
        let val_str = self.to_val(val_str, next)?;
        let empty = PrimExpr::CallBuiltin(
            Function::Binop(ast::Binop::EQ),
            smallvec![val_str, PrimVal::StrLit(b"")],
        );
        let empty = self.to_val(empty, next)?;
        let missing = PrimExpr::CallBuiltin(Function::Unop(ast::Unop::Not), smallvec![present]);
        let missing = self.to_val(missing, next)?;
        let either = PrimExpr::CallBuiltin(
            Function::Binop(ast::Binop::Plus),
            smallvec![missing, empty],
        );
        let either = self.to_val(either, next)?;
        Ok((
            next,
            PrimExpr::CallBuiltin(
                Function::Binop(ast::Binop::GT),
                smallvec![either, PrimVal::ILit(0)],
            ),
        ))
    }

    /// Converts an expression in a position that takes a regex (the right side of `~`, builtin
    /// arguments): a regex literal there is the pattern string rather than `$0 ~ /re/`.
    fn convert_regex_arg<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        match expr {
            Expr::PatLit(s) => Ok((current_open, PrimVal::StrLit(s))),
            _ => self.convert_val(expr, current_open),
        }
    }

    fn convert_val_inner<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
        in_cond: bool,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        let (next_open, e) = self.convert_expr_inner(expr, current_open, in_cond)?;
        Ok((next_open, self.to_val(e, next_open)?))
    }

    // Handles "break", "continue" statements.
    /// Hidden global variables: (exiting flag, pending exit code).
    fn exit_vars(&mut self) -> (Ident, Ident) {
        let exiting = self.get_identifier(&I::from_static("%exiting"));
        let code = self.get_identifier(&I::from_static("%exit_code"));
        (exiting, code)
    }

    fn end_entry(&mut self) -> NodeIx {
        match self.f.end_entry {
            Some(entry) => entry,
            None => {
                let entry = self.f.cfg.add_node(Default::default());
                self.f.end_entry = Some(entry);
                entry
            }
        }
    }

    /// A node that exits the program with the pending exit code.
    fn exit_now_node(&mut self) -> Result<NodeIx> {
        let (_, code) = self.exit_vars();
        let node = self.f.cfg.add_node(Default::default());
        self.add_stmt(
            node,
            PrimStmt::AsgnVar(
                Ident::unused(),
                PrimExpr::CallBuiltin(builtins::Function::Exit, smallvec![PrimVal::Var(code)]),
            ),
        )?;
        Ok(node)
    }

    /// Where control goes when exiting: the END block from BEGIN or the main loop, the exit
    /// of the function (returning to the caller, which checks the exiting flag) from a user
    /// defined function, and the program exit from the END block.
    fn exit_target(&mut self) -> Result<NodeIx> {
        if self.f.in_end {
            self.exit_now_node()
        } else if self.f.name.is_main() {
            Ok(self.end_entry())
        } else {
            let node = self.f.cfg.add_node(Default::default());
            self.add_stmt(
                node,
                PrimStmt::AsgnVar(self.f.ret, PrimExpr::Val(PrimVal::Var(Ident::unused()))),
            )?;
            self.f.cfg.add_edge(node, self.f.exit, Transition::null());
            self.seal(node);
            Ok(node)
        }
    }

    /// `exit [code]`. When the END block must run first (POSIX), record the exit code and jump
    /// to END, returning an unreachable node for any statements following `exit`. `exit` without
    /// a code exits with 0, or with the code of an earlier `exit` when called from END.
    fn do_exit<'c>(&mut self, code: Option<&'c Expr<'c, 'b, I>>, current_open: NodeIx) -> Result<NodeIx> {
        let (current_open, code_v) = match code {
            Some(code) => {
                let (next, v) = self.convert_val(code, current_open)?;
                (next, Some(v))
            }
            None => (current_open, None),
        };
        if !self.ctx.exit_to_end {
            let code_v = code_v.unwrap_or(PrimVal::ILit(0));
            self.add_stmt(
                current_open,
                PrimStmt::AsgnVar(
                    Ident::unused(),
                    PrimExpr::CallBuiltin(builtins::Function::Exit, smallvec![code_v]),
                ),
            )?;
            return Ok(current_open);
        }
        let (exiting, code) = self.exit_vars();
        let code_e = match code_v {
            Some(v) => Some(PrimExpr::CallBuiltin(builtins::Function::ToInt, smallvec![v])),
            None if self.f.in_end => None,
            None => Some(PrimExpr::Val(PrimVal::ILit(0))),
        };
        if let Some(code_e) = code_e {
            self.add_stmt(current_open, PrimStmt::AsgnVar(code, code_e))?;
        }
        self.add_stmt(current_open, PrimStmt::AsgnVar(exiting, PrimExpr::Val(PrimVal::ILit(1))))?;
        let target = self.exit_target()?;
        self.jump_to(current_open, target)
    }

    /// Awk numbers are doubles, while zawk computes with 64-bit integers where it can. A sum or
    /// difference that adds a constant (`i + 1`, `NR - 1`, `i++`) stays an integer: overflowing
    /// would take more than 2^31 steps. Otherwise (`a + b`, as in a Fibonacci sequence) it may grow
    /// without bound, so `v` is converted to a float, which makes the operation a float one.
    /// A chain of three or more concatenations (`$1 "," $2`), as a single call with all the
    /// operands, so that the result is built in one go rather than one `Concat` node per operand
    /// (see `Str::concat_n`). The operands are evaluated left to right, as in nested binary
    /// concatenations.
    fn convert_concat<'c>(
        &mut self,
        expr: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        let mut leaves = SmallVec::new();
        flatten_concat(expr, &mut leaves);
        let mut next = current_open;
        let mut vs = SmallVec::with_capacity(leaves.len());
        for (i, leaf) in leaves.iter().enumerate() {
            let (n, v) = self.convert_str_operand(leaf, next, /*ofmt=*/ false)?;
            next = n;
            vs.push(self.snapshot_before(v, &leaves[i + 1..], next)?);
        }
        Ok((next, PrimExpr::CallBuiltin(builtins::Function::Binop(ast::Binop::Concat), vs)))
    }

    fn widen_sum_operand(
        &mut self,
        op: ast::Binop,
        v: PrimVal<'b>,
        other: &PrimVal<'b>,
        current_open: NodeIx,
    ) -> Result<PrimVal<'b>> {
        let is_const = |v: &PrimVal| matches!(v, PrimVal::ILit(_) | PrimVal::FLit(_));
        if !matches!(op, ast::Binop::Plus | ast::Binop::Minus) || is_const(&v) || is_const(other) {
            return Ok(v);
        }
        self.to_val(
            PrimExpr::CallBuiltin(builtins::Function::ToFloat, smallvec![v]),
            current_open,
        )
    }

    /// Converts an array subscript. Unlike elsewhere (see `widen_sum_operand`), a sum or
    /// difference used directly as a subscript stays an integer operation, as with `a[i + j]`:
    /// an integer key keeps the array integer-keyed, and such a key never gets near overflowing.
    fn convert_index<'c>(
        &mut self,
        ix: &'c Expr<'c, 'b, I>,
        current_open: NodeIx,
        in_cond: bool,
    ) -> Result<(NodeIx, PrimVal<'b>)> {
        if let Expr::Binop(op @ (ast::Binop::Plus | ast::Binop::Minus), e1, e2) = ix {
            let (next, v1) = self.convert_val_inner(e1, current_open, in_cond)?;
            let (next, v2) = self.convert_val_inner(e2, next, in_cond)?;
            let sum = PrimExpr::CallBuiltin(builtins::Function::Binop(*op), smallvec![v1, v2]);
            return Ok((next, self.to_val(sum, next)?));
        }
        self.convert_val_inner(ix, current_open, in_cond)
    }

    /// Ends `current_open` with a jump to `target` (for `exit`, `break`, `continue`, `next` and
    /// `return`), and returns a new block for the statements that follow, which never run. A
    /// never-taken branch into that block keeps those statements well-formed.
    fn jump_to(&mut self, current_open: NodeIx, target: NodeIx) -> Result<NodeIx> {
        let never = self.fresh_local();
        self.add_stmt(current_open, PrimStmt::AsgnVar(never, PrimExpr::Val(PrimVal::ILit(0))))?;
        let dead = self.f.cfg.add_node(Default::default());
        self.f.cfg.add_edge(current_open, dead, Transition::new(PrimVal::Var(never)));
        self.f.cfg.add_edge(current_open, target, Transition::null());
        self.seal(current_open);
        Ok(dead)
    }

    /// Evaluate a call to a user defined function, then leave if it called `exit`.
    fn after_udf_call(&mut self, call: PrimExpr<'b>, current_open: NodeIx) -> Result<(NodeIx, PrimExpr<'b>)> {
        let res = self.fresh_local();
        self.add_stmt(current_open, PrimStmt::AsgnVar(res, call))?;
        let (exiting, _) = self.exit_vars();
        let target = self.exit_target()?;
        let next = self.f.cfg.add_node(Default::default());
        self.f.cfg.add_edge(current_open, target, Transition::new(PrimVal::Var(exiting)));
        self.f.cfg.add_edge(current_open, next, Transition::null());
        self.seal(current_open);
        Ok((next, PrimExpr::Val(PrimVal::Var(res))))
    }

    fn do_break_continue(&mut self, current_open: NodeIx, is_break: bool) -> Result<NodeIx> {
        // `break` leaves the innermost loop or switch, `continue` restarts the innermost loop.
        let ix = if is_break {
            self.f.loop_ctx.len().checked_sub(1)
        } else {
            self.f.loop_ctx.iter().rposition(|c| !c.3)
        };
        // The toplevel loop over the input records is not a loop of the program.
        let ix = ix.filter(|ix| Some(self.f.loop_ctx[*ix].0) != self.f.toplevel_header);
        match ix.map(|ix| self.f.loop_ctx[ix]) {
            Some((header, footer, update, _)) => {
                // Continue statements jump to the update node or beginning.
                let target = match (is_break, update) {
                    (true, _) => footer,
                    (false, Some(update)) => update,
                    (false, None) => header,
                };
                self.jump_to(current_open, target)
            }
            None if is_break => syntax_err!("`break' is not allowed outside a loop or switch"),
            None => syntax_err!("`continue' is not allowed outside a loop"),
        }
    }

    /// `switch (e) { ... }`: `e` is evaluated once and compared with the case labels in order
    /// (with `==` for numbers and strings, with `~` for regexes). Control jumps to the body of the
    /// first matching case, or of `default`, and falls through the following bodies until a
    /// `break`.
    fn do_switch<'c>(
        &mut self,
        e: &'c Expr<'c, 'b, I>,
        cases: &'c [(Option<&'c Expr<'c, 'b, I>>, &'c Stmt<'c, 'b, I>)],
        current_open: NodeIx,
    ) -> Result<NodeIx> {
        // Duplicate labels are rejected by the parser.
        let default = cases.iter().position(|(l, _)| l.is_none());
        let (mut current_open, num_v) = self.convert_val(e, current_open)?;
        // An uninitialized variable is "" when compared with a string or a regex, and 0 otherwise
        // (see the `uninit` module).
        let str_case = cases
            .iter()
            .any(|(l, _)| matches!(l, Some(Expr::StrLit(_) | Expr::PatLit(_))));
        let str_v = match self.uninit_flag(e) {
            Some(flag) if str_case => {
                let flag = PrimVal::Var(self.get_identifier(&flag));
                let (next, v) = self.str_or_empty(flag, num_v.clone(), /*ofmt=*/ false, current_open)?;
                current_open = next;
                v
            }
            _ => num_v.clone(),
        };

        let footer = self.f.cfg.add_node(Default::default());
        self.f.loop_ctx.push((footer, footer, None, true));
        let mut bodies = Vec::with_capacity(cases.len());
        for (_, body) in cases.iter() {
            bodies.push(self.standalone_block(body)?);
        }
        self.f.loop_ctx.pop().unwrap();

        // Test the labels in order.
        for ((label, _), (b_start, _)) in cases.iter().zip(bodies.iter()) {
            let (op, v, lit) = match label {
                None => continue,
                Some(Expr::ILit(n)) => (ast::Binop::EQ, &num_v, PrimVal::ILit(*n)),
                Some(Expr::FLit(f)) => (ast::Binop::EQ, &num_v, PrimVal::FLit(*f)),
                Some(Expr::StrLit(s)) => (ast::Binop::EQ, &str_v, PrimVal::StrLit(s)),
                Some(Expr::PatLit(s)) => (ast::Binop::IsMatch, &str_v, PrimVal::StrLit(s)),
                Some(_) => return err!("invalid case label in switch body"),
            };
            let cond = PrimExpr::CallBuiltin(builtins::Function::Binop(op), smallvec![v.clone(), lit]);
            let cond = self.to_val(cond, current_open)?;
            let next = self.f.cfg.add_node(Default::default());
            self.f.cfg.add_edge(current_open, *b_start, Transition::new(cond));
            self.f.cfg.add_edge(current_open, next, Transition::null());
            current_open = next;
        }
        // No label matched.
        let fallback = default.map_or(footer, |i| bodies[i].0);
        self.f.cfg.add_edge(current_open, fallback, Transition::null());

        // Each body falls through to the next one.
        for (i, (_, b_end)) in bodies.iter().enumerate() {
            let next = bodies.get(i + 1).map_or(footer, |b| b.0);
            self.guarded_else(*b_end, next);
        }
        Ok(footer)
    }

    fn set_cond(&mut self, current_open: NodeIx, cond: usize, cond_val: i64) -> Result<()> {
        let cond_ident = self.get_cond(cond);
        self.add_stmt(
            current_open,
            PrimStmt::AsgnVar(cond_ident, PrimExpr::Val(PrimVal::ILit(cond_val))),
        )
    }

    // Handles "next", "nextfile" statements.
    fn do_next(&mut self, current_open: NodeIx, is_next_file: bool) -> Result<NodeIx> {
        // The END block runs after the main loop, so jumping back to its header would rerun END
        // forever.
        if self.f.in_end {
            return syntax_err!(
                "`{}' used in END action",
                if is_next_file { "nextfile" } else { "next" }
            );
        }
        if let Some(header) = self.f.toplevel_header {
            if is_next_file {
                self.add_stmt(
                    current_open,
                    PrimStmt::AsgnVar(
                        Ident::unused(),
                        PrimExpr::CallBuiltin(builtins::Function::NextFile, smallvec![]),
                    ),
                )?;
            }
            self.jump_to(current_open, header)
        } else if !matches!(self.f.name, FunctionName::Named(_)) {
            // Outside a function, the toplevel loop has not started yet: this is BEGIN.
            syntax_err!(
                "`{}' used in BEGIN action",
                if is_next_file { "nextfile" } else { "next" }
            )
        } else {
            err!(
                "Cannot use `{}` from outside of the toplevel loop! \
                 Note that zawk does not support `next` or `nextfile` from inside functions.",
                if is_next_file { "nextfile" } else { "next" }
            )
        }
    }

    fn make_loop<'c>(
        &mut self,
        body: &'c Stmt<'c, 'b, I>,
        update: Option<&'c Stmt<'c, 'b, I>>,
        current_open: NodeIx,
        is_do: bool,
        is_toplevel: bool,
    ) -> Result<(
        NodeIx, // header
        NodeIx, // body header
        NodeIx, // body footer
        NodeIx, // footer = next open
    )> {
        // Create header and footer nodes.
        let h = self.f.cfg.add_node(Default::default());
        let f = self.f.cfg.add_node(Default::default());
        // Create update node if necessary.
        let u = match update {
            Some(update) => {
                let u = Some(self.f.cfg.add_node(Default::default()));
                self.convert_stmt(update, u.unwrap())?;
                u
            },
            None => None,
        };
        self.f.loop_ctx.push((h, f, u, false));
        if is_toplevel {
            self.f.toplevel_header = Some(h);
        }

        // The body is a standalone graph.
        let (b_start, b_end) = if let Some(u) = u {
            let (start, mid) = self.standalone_block(body)?;
            self.f
                .cfg
                .add_edge(mid, u, Transition::null());
            let end = u;
            (start, end)
        } else {
            self.standalone_block(body)?
        };

        // do-while loops start by running the loop body.
        // The last few edges here are added after make_loop returns to convert_stmt.
        if is_do {
            // Current => Body => Header => Body => Footer
            //                      ^       |
            //                      ^--------
            self.f
                .cfg
                .add_edge(current_open, b_start, Transition::null());
            self.f.cfg.add_edge(b_end, h, Transition::null());
        } else {
            // Current => Header => Body => Footer
            //             ^         |
            //             ^---------
            self.f.cfg.add_edge(current_open, h, Transition::null());
            self.f.cfg.add_edge(b_end, h, Transition::null());
        }
        self.f.loop_ctx.pop().unwrap();
        Ok((h, b_start, b_end, f))
    }

    #[allow(clippy::wrong_self_convention)]
    /// Operands are evaluated from left to right: in `i + i++`, `print i, i++` or `i (i = 9)`, the
    /// first operand is the value of `i` before the later operands change it. A variable operand
    /// is read where it is used, so when a later operand may assign variables, `v` is copied into
    /// a fresh local first.
    fn snapshot_before<'c>(
        &mut self,
        v: PrimVal<'b>,
        later: &[&'c Expr<'c, 'b, I>],
        current_open: NodeIx,
    ) -> Result<PrimVal<'b>> {
        if !matches!(v, PrimVal::Var(_)) || !later.iter().any(|e| self.may_assign(e)) {
            return Ok(v);
        }
        let f = self.fresh_local();
        self.add_stmt(current_open, PrimStmt::AsgnVar(f, PrimExpr::Val(v)))?;
        Ok(PrimVal::Var(f))
    }

    /// Whether evaluating `e` may assign a variable: an assignment, an increment, getline, sub or
    /// gsub, or a call of a user-defined function (which may assign globals).
    fn may_assign<'c>(&self, e: &'c Expr<'c, 'b, I>) -> bool {
        use builtins::Function::{GSub, Sub};
        use Expr::*;
        match e {
            ILit(_) | FLit(_) | StrLit(_) | PatLit(_) | Var(_) | Cond(_) => false,
            Unop(_, e) => self.may_assign(e),
            Binop(_, l, r) | Index(l, r) | And(l, r) | Or(l, r) | Nullish(l, r) => {
                self.may_assign(l) || self.may_assign(r)
            }
            ITE(c, t, f) => self.may_assign(c) || self.may_assign(t) || self.may_assign(f),
            Assign(..) | AssignOp(..) | Inc { .. } | Getline { .. } | ReadStdin => true,
            Call(f, args) => {
                let assigns = match f {
                    Either::Left(name) if name.is_sprintf() => false,
                    Either::Left(name) => {
                        self.func_table.contains_key(&FunctionName::Named(name.clone()))
                            || !matches!(
                                builtins::Function::try_from(name.clone()),
                                Ok(bi) if !matches!(bi, Sub | GSub)
                            )
                    }
                    Either::Right(bi) => matches!(bi, Sub | GSub),
                };
                assigns || args.iter().any(|a| self.may_assign(a))
            }
        }
    }

    fn to_val(&mut self, exp: PrimExpr<'b>, current_open: NodeIx) -> Result<PrimVal<'b>> {
        Ok(if let PrimExpr::Val(v) = exp {
            v
        } else {
            let f = self.fresh_local();
            self.add_stmt(current_open, PrimStmt::AsgnVar(f, exp))?;
            PrimVal::Var(f)
        })
    }

    fn call<'c>(
        &mut self,
        current_open: NodeIx,
        fname: &Either<I, builtins::Function>,
        args: &'c [&'c Expr<'c, 'b, I>],
    ) -> Result<(NodeIx, PrimExpr<'b>)> {
        // Handle call expressions. This is pretty complicated because AWK has several rules that
        // "fill in missing arguments".
        let bi = match fname {
            Either::Left(fname) if fname.is_sprintf() => {
                // sprintf handled even more specially, because it is the one truly var-arg
                // function that occurs in expression position.
                return self.do_sprintf(args, current_open);
            }
            // A user-defined function shadows a zawk stdlib function of the same name.
            Either::Left(fname)
            if self.func_table.contains_key(&FunctionName::Named(fname.clone())) =>
                {
                    Either::Left(fname.clone())
                }
            Either::Left(fname) => {
                if let Ok(bi) = builtins::Function::try_from(fname.clone()) {
                    // Okay, there's a builtin in here.
                    Either::Right(bi)
                } else {
                    // We'll keep this as a raw identifier. Below, we'll check if it's a UDF, or if
                    // the function does not exist.
                    Either::Left(fname.clone())
                }
            }
            // Various parts of the AST are parsed directly into the builtin variant, we propagate
            // that usage here.
            Either::Right(bi) => Either::Right(*bi),
        };
        // index(s, /re/): a regex literal as a value is `$0 ~ /re/`, so this would silently search
        // for "0" or "1". gawk rejects it, so do we.
        if let (
            Either::Right(f @ (builtins::Function::SubstrIndex | builtins::Function::SubstrLastIndex)),
            [_, Expr::PatLit(_)],
        ) = (&bi, args)
        {
            let name = if *f == builtins::Function::SubstrIndex { "index" } else { "last_index" };
            return err!(
                "{}: regexp constant as second argument is not allowed; use `match(s, /re/)` to find the position of a regex match",
                name
            );
        }
        // length and length() => length($0)
        if let (Either::Right(builtins::Function::Length), []) = (&bi, args) {
            return self.call(
                current_open,
                &Either::Right(builtins::Function::Length),
                &[&Expr::Unop(Unop::Column, &Expr::ILit(0))],
            );
        }
        if let (Either::Right(builtins::Function::TypeOfVariable), [arg]) = (&bi, args) {
            if let Some(res) = self.type_of_dynamic(arg, current_open)? {
                return Ok(res);
            }
        }
        // typeof(x) of a variable that is never read anywhere: "untyped", as in gawk.
        let bi = match (bi, args) {
            (Either::Right(builtins::Function::TypeOfVariable), [Expr::Var(x) | Expr::Index(Expr::Var(x), _)])
            if !self.scalar_referenced(x) =>
                {
                    Either::Right(builtins::Function::TypeOfUntyped)
                }
            (bi, _) => bi,
        };
        let mut prim_args = SmallVec::with_capacity(args.len());
        let mut open = current_open;
        // The second argument of these is an array subscript.
        use builtins::Function::{Contains, Delete, IncMap};
        let key_arg = matches!(bi, Either::Right(IncMap | Contains | Delete));
        for (i, a) in args.iter().enumerate() {
            let (next, v) = if key_arg && i == 1 {
                self.convert_index(a, open, /*in_cond=*/ false)?
            } else if matches!(bi, Either::Right(builtins::Function::Length)) {
                self.convert_str_operand(a, open, /*ofmt=*/ false)?
            } else if matches!(bi, Either::Right(_)) {
                // Builtins take regex literals as patterns (split, sub, match, ...); for user
                // defined functions, as in gawk, a regex literal argument is `$0 ~ /re/`.
                self.convert_regex_arg(a, open)?
            } else {
                self.convert_val(a, open)?
            };
            open = next;
            prim_args.push(v);
        }
        match bi {
            Either::Left(fname) => {
                return if let Some(i) = self.func_table.get(&FunctionName::Named(fname.clone())) {
                    // For field separator optimizations, any UDF calls in the BEGIN block of main
                    // causes fallback to the generic regex-based splitter.
                    //
                    // TODO: this is pretty crude. It would be better to handle more cases here.
                    self.f
                        .vars
                        .entry(None)
                        .or_insert_with(Vec::new)
                        .push((current_open.index(), None));
                    // Extra arguments are evaluated (for their side effects) but not passed: they
                    // would otherwise land in the slots of the callee's `local` variables, which
                    // are filled with nulls instead.
                    prim_args.truncate(self.ctx.params[*i as usize]);
                    let call = PrimExpr::CallUDF(*i, prim_args);
                    if self.ctx.exit_to_end {
                        // `exit` inside the callee returns early with the exiting flag set.
                        self.after_udf_call(call, open)
                    } else {
                        Ok((open, call))
                    }
                } else {
                    err!("Call to unknown function \"{}\"", fname)
                };
            }
            // Now to "fill in the extras."
            Either::Right(mut bi) => {
                // todo performance enhancement
                // split(string, array) => split(string, array, FS)
                if bi == builtins::Function::Split && args.len() == 2 {
                    let fs = self.fresh_local();
                    self.add_stmt(
                        current_open,
                        PrimStmt::AsgnVar(fs, PrimExpr::LoadBuiltin(builtins::Variable::FS)),
                    )?;
                    prim_args.push(PrimVal::Var(fs));
                }
                // split(s, arr, fs, seps) => gawk's form that also returns the separators
                if bi == builtins::Function::Split && args.len() == 4 {
                    bi = builtins::Function::SplitSeps;
                }
                // patsplit(s, arr, re) => patsplit(s, arr, re, <unused seps>)
                if bi == builtins::Function::PatSplit && args.len() == 3 {
                    prim_args.push(PrimVal::Var(self.fresh_local()));
                }
                // match(s, re, arr) => gawk's capturing form
                if bi == builtins::Function::Match && args.len() == 3 {
                    bi = builtins::Function::MatchArr;
                }

                // join_fields(start, end) => join_{c,t}sv (if in csv/tsv output mode)
                // join_fields(start, end) => join_fields(start, end, OFS) (otherwise)
                if bi == builtins::Function::JoinCols && args.len() == 2 {
                    match self.ctx.esc {
                        Escaper::CSV => bi = builtins::Function::JoinCSV,
                        Escaper::TSV => bi = builtins::Function::JoinTSV,
                        Escaper::Identity => {
                            let fs = self.fresh_local();
                            self.add_stmt(
                                current_open,
                                PrimStmt::AsgnVar(
                                    fs,
                                    PrimExpr::LoadBuiltin(builtins::Variable::OFS),
                                ),
                            )?;
                            prim_args.push(PrimVal::Var(fs));
                        }
                    }
                }
                // Fill params if absent
                let args_len = args.len();
                match bi {
                    // getenv(name) => getenv(name, default_value)
                    builtins::Function::GetEnv if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // eval() => eval(formula, context)
                    builtins::Function::Eval if args_len == 1 => {
                        prim_args.push(PrimVal::Var(Ident::unused()));
                    }
                    // mkpass() => mkpass(8)
                    builtins::Function::MkPassword if args_len == 0 => {
                        prim_args.push(PrimVal::ILit(8));
                    }
                    // fake(data) => fake(data, "")
                    builtins::Function::Fake if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // rgb2hex(r) => rgb2hex(r,0,0)
                    builtins::Function::Rgb2Hex if args_len == 1 => {
                        prim_args.push(PrimVal::ILit(0));
                        prim_args.push(PrimVal::ILit(0));
                    }
                    // rgb2hex(r,g) => rgb2hex(r,g,0)
                    builtins::Function::Rgb2Hex if args_len == 2 => {
                        prim_args.push(PrimVal::ILit(0));
                    }
                    // last_part (path) => last_path(path, "")
                    builtins::Function::LastPart if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // bf_insert (item) => bf_insert(item, "_")
                    builtins::Function::BloomFilterInsert if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b"_"));
                    }
                    // bf_contains(item) => bf_contains(item, "_")
                    builtins::Function::BloomFilterContains if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b"_"));
                    }
                    // bf_icontains(item) => bf_icontains(item, "_")
                    builtins::Function::BloomFilterContainsWithInsert if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b"_"));
                    }
                    // pairs (s) => pair(s, ",", "=")
                    builtins::Function::Pairs if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b","));
                        prim_args.push(PrimVal::StrLit(b"="));
                    }
                    // pairs (s, ",") => pair(s, ",", "=")
                    builtins::Function::Pairs if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b"="));
                    }
                    // pad (s, n) => pad(s, len, " ")
                    builtins::Function::PadLeft if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b" "));
                    }
                    builtins::Function::PadRight if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b" "));
                    }
                    builtins::Function::PadBoth if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b" "));
                    }
                    // snowflake() => snowflake(1);
                    builtins::Function::SnowFlake if args_len == 0 => {
                        prim_args.push(PrimVal::ILit(1));
                    }
                    // datetime() => datetime(timestamp_now);
                    builtins::Function::DateTime if args_len == 0 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // uniq(arr) => uniq(arr, param);
                    builtins::Function::Uniq if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // uuid() => uuid("v4");
                    builtins::Function::Uuid if args_len == 0 => {
                        prim_args.push(PrimVal::StrLit(b"v4"));
                    }
                    // _median(a) => stat(a, QUANTILE, 0.5); _quantile(a, q) => stat(a, QUANTILE, q);
                    // _var(a) => stat(a, VAR, 0); _stddev(a) => stat(a, STDDEV, 0);
                    // _mode(a) => stat(a, MODE, 0).
                    builtins::Function::ArrayMedian
                    | builtins::Function::ArrayQuantile
                    | builtins::Function::ArrayVar
                    | builtins::Function::ArrayStddev
                    | builtins::Function::ArrayMode => {
                        use crate::runtime::array_stats::*;
                        use builtins::Function::*;
                        let expected = if bi == ArrayQuantile { 2 } else { 1 };
                        if args_len != expected {
                            return Err(builtins::arity_error(&call_name(fname), args_len, expected));
                        }
                        let q = if bi == ArrayQuantile { prim_args.pop().unwrap() } else { PrimVal::FLit(0.5) };
                        let kind = match bi {
                            ArrayVar => STAT_VAR,
                            ArrayStddev => STAT_STDDEV,
                            ArrayMode => STAT_MODE,
                            _ => STAT_QUANTILE,
                        };
                        prim_args.push(PrimVal::ILit(kind));
                        prim_args.push(q);
                        bi = ArrayStat;
                    }
                    // _join(arr) => join(arr, " ");
                    builtins::Function::IntMapJoin if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b" "));
                    }
                    // substr(s, a) => substr(s, a, INT_MAX); as we always clamp the second value to
                    // the length of s.
                    builtins::Function::Substr if args_len == 2 => {
                        // We clamp indexes anyway, we'll just put a big number in as the
                        // rightmost index.
                        prim_args.push(PrimVal::ILit(i64::MAX));
                    }
                    // fflush() => fflush(""): flush all output.
                    builtins::Function::Fflush if args_len == 0 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // strftime(format, timestamp, utc) => strftime(format, timestamp, flags):
                    // the timestamp defaults to the current time, and the format of strftime() is
                    // PROCINFO["strftime"] (see `date_time::awk_strftime`).
                    builtins::Function::Strftime if args_len < 3 => {
                        use crate::runtime::date_time::{STRFTIME_NOW, STRFTIME_PROCINFO_FORMAT};
                        if args_len == 0 {
                            prim_args.push(PrimVal::StrLit(b""));
                        }
                        if args_len < 2 {
                            prim_args.push(PrimVal::ILit(STRFTIME_NOW));
                        }
                        let flags = if args_len == 0 { STRFTIME_PROCINFO_FORMAT } else { 0 };
                        prim_args.push(PrimVal::ILit(flags));
                    }
                    // The utc flag is true when it is nonzero or a nonempty string: `!!utc`, which
                    // is STRFTIME_UTC.
                    builtins::Function::Strftime => {
                        use builtins::Function::Unop;
                        let utc = prim_args.pop().unwrap();
                        let not = PrimExpr::CallBuiltin(Unop(ast::Unop::Not), smallvec![utc]);
                        let not = self.to_val(not, open)?;
                        let utc = PrimExpr::CallBuiltin(Unop(ast::Unop::Not), smallvec![not]);
                        prim_args.push(self.to_val(utc, open)?);
                    }
                    // mktime(date_text [, utc [, timezone]]): the utc flag defaults to 0 and is true
                    // when it is nonzero or a nonempty string (`!!utc`), as in gawk; the timezone
                    // defaults to MKTIME_LOCAL_TIMEZONE.
                    builtins::Function::Mktime if (1..=3).contains(&args_len) => {
                        use builtins::Function::Unop;
                        let timezone = if args_len == 3 { prim_args.pop() } else { None };
                        if args_len == 1 {
                            prim_args.push(PrimVal::ILit(0));
                        } else {
                            let utc = prim_args.pop().unwrap();
                            let not = PrimExpr::CallBuiltin(Unop(ast::Unop::Not), smallvec![utc]);
                            let not = self.to_val(not, open)?;
                            let utc = PrimExpr::CallBuiltin(Unop(ast::Unop::Not), smallvec![not]);
                            prim_args.push(self.to_val(utc, open)?);
                        }
                        prim_args.push(timezone.unwrap_or(PrimVal::ILit(
                            crate::runtime::date_time::MKTIME_LOCAL_TIMEZONE as Int,
                        )));
                    }
                    // trim(s) => trim(s, " ");
                    builtins::Function::Trim if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b" "));
                    }
                    // truncate($1, 10) => truncate($1, 10, "...");
                    builtins::Function::Truncate if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b"..."));
                    }
                    // min($1, 10) => min($1, 10, "");
                    builtins::Function::Min if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // jev(record, instructions) => jev(to_json(record), instructions, jev_threshold)
                    // jev_prob(record, instructions) => jev_prob(to_json(record), instructions)
                    // jev_choice(record, instructions, options) => jev_choice(to_json(record), instructions, to_json(options))
                    // Arity is checked later; with fewer than 2 args, skip the rewrite so the check reports it.
                    builtins::Function::Jev | builtins::Function::JevProb | builtins::Function::JevChoice | builtins::Function::JevScore
                        if args_len >= 2 =>
                    {
                        let json_args: &[usize] = if args_len >= 3 { &[0, 2] } else { &[0] };
                        for &i in json_args {
                            let arg = prim_args[i].clone();
                            let json = PrimExpr::CallBuiltin(builtins::Function::ToJson, smallvec![arg]);
                            prim_args[i] = self.to_val(json, open)?;
                        }
                        if bi == builtins::Function::Jev && args_len == 2 {
                            let threshold = Expr::Var(I::from_static("jev_threshold"));
                            let (next, v) = self.convert_val(&threshold, open)?;
                            open = next;
                            prim_args.push(v);
                        }
                    }
                    // max($1, 10) => max($1, 10, "");
                    builtins::Function::Max if args_len == 2 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // seq(10) => seq(1,1 10);
                    builtins::Function::Seq if args_len == 1 => {
                        let max = prim_args.pop().unwrap();
                        prim_args.push(PrimVal::ILit(0));
                        prim_args.push(PrimVal::ILit(1));
                        prim_args.push(max);
                    }
                    // seq(1, 10) => seq(1,1 10);
                    builtins::Function::Seq if args_len == 2 => {
                        let max = prim_args.pop().unwrap();
                        prim_args.push(PrimVal::ILit(1));
                        prim_args.push(max);
                    }
                    // asort(arr) => asort(arr, arr, ""): sort in place.
                    // asort(src, dst) => asort(src, dst, ""): "" is the default order.
                    builtins::Function::Asort if args_len == 1 || args_len == 2 => {
                        if args_len == 1 {
                            prim_args.push(prim_args[0].clone());
                        }
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // asorti(arr) => asorti(arr, arr, "@ind_str_asc"): sort in place.
                    // asorti(src, dst) => asorti(src, dst, "@ind_str_asc");
                    builtins::Function::Asorti if args_len == 1 || args_len == 2 => {
                        if args_len == 1 {
                            prim_args.push(prim_args[0].clone());
                        }
                        prim_args.push(PrimVal::StrLit(b"@ind_str_asc"));
                    }
                    // send_mail(from, to, subject, body) => send_mail(from, to, subject, body, "");
                    builtins::Function::SendMail if args_len == 4 => {
                        prim_args.push(PrimVal::StrLit(b""));
                    }
                    // smtp_send(url, from, to, subject, body) => send_mail(from, to, subject, body, url);
                    builtins::Function::SmtpSend if args_len == 5 => {
                        let url = prim_args.remove(0);
                        prim_args.push(url);
                        bi = builtins::Function::SendMail;
                    }
                    // http_get(url) => http_get(url, headers), with empty headers: a fresh local
                    // that `Function::feedback` types as a map.
                    builtins::Function::HttpGet if args_len == 1 => {
                        prim_args.push(PrimVal::Var(self.fresh_local()));
                    }
                    // sqlite_query(db, sql) => sqlite_query(db, sql, params), with no parameters: a
                    // fresh local that `Function::feedback` types as a map; the same for the other
                    // database functions.
                    builtins::Function::SqliteQuery | builtins::Function::SqliteExecute
                    | builtins::Function::LibsqlQuery | builtins::Function::LibsqlExecute
                    | builtins::Function::MysqlQuery | builtins::Function::MysqlExecute
                    | builtins::Function::PgQuery | builtins::Function::PgExecute
                        if args_len == 2 =>
                    {
                        prim_args.push(PrimVal::Var(self.fresh_local()));
                    }
                    // http_post(url) => http_post(url, "", headers);
                    builtins::Function::HttpPost if args_len == 1 => {
                        prim_args.push(PrimVal::StrLit(b""));
                        prim_args.push(PrimVal::Var(self.fresh_local()));
                    }
                    // http_post(url, body) => http_post(url, body, headers);
                    builtins::Function::HttpPost if args_len == 2 => {
                        prim_args.push(PrimVal::Var(self.fresh_local()));
                    }
                    _ => {}
                }

                // srand() => the special "reseed rng" function
                if bi == builtins::Function::Srand && args.is_empty() {
                    bi = builtins::Function::ReseedRng;
                }

                // sub/gsub are the most complicated cases. Why? Because they take their last
                // argument as an out-param. Not only is the 3rd argument "implicitly $0", but we
                // assign into $0 if that happens.
                //
                // Even when the third argument is provided, we still have to insert an assignment
                // expression in the appropriate locations.
                if let builtins::Function::Sub | builtins::Function::GSub = bi {
                    let assignee = match args.len() {
                        3 => args[2],
                        2 => {
                            // If a third argument isn't provided, we assume you mean $0.
                            let e = &Expr::Unop(ast::Unop::Column, &Expr::ILit(0));
                            let (next, v) = self.convert_val(e, open)?;
                            open = next;
                            prim_args.push(v);
                            e
                        }
                        n => return Err(builtins::arity_error(&call_name(fname), n, 3)),
                    };
                    // Easy case! How delighful
                    if let Expr::Var(_) = assignee {
                        self.mark_assigned(assignee, None, open)?;
                        return Ok((open, PrimExpr::CallBuiltin(bi, prim_args)));
                    }
                    // We got something like sub(x, y, m[z]);
                    // We allocate fresh variables for the initial value of the assignee
                    // and the result of the call to (g)sub.
                    //
                    // We do the computation, then assign the substituted string back to the
                    // assignee expression only if a substitution happened, yielding the saved
                    // result. Writing back unconditionally would rebuild $0 (with OFS) even
                    // when nothing matched.
                    let to_set = self.fresh_local();
                    let res = self.fresh_local();
                    let last_arg = mem::replace(&mut prim_args[2], PrimVal::Var(to_set));
                    self.add_stmt(open, PrimStmt::AsgnVar(to_set, PrimExpr::Val(last_arg)))?;
                    prim_args[2] = PrimVal::Var(to_set);
                    self.add_stmt(
                        open,
                        PrimStmt::AsgnVar(res, PrimExpr::CallBuiltin(bi, prim_args)),
                    )?;
                    let to_set_var = PrimExpr::Val(PrimVal::Var(to_set));
                    let set = self.f.cfg.add_node(Default::default());
                    let next = self.f.cfg.add_node(Default::default());
                    self.f.cfg.add_edge(open, set, Transition::new(PrimVal::Var(res)));
                    self.f.cfg.add_edge(open, next, Transition::null());
                    let (set_end, _) = match assignee {
                        Expr::Unop(_, _) => self.do_assign(assignee, |_| to_set_var, set),
                        Expr::Index(arr, ix) => self.do_assign_index(
                            arr,
                            ix,
                            |_, _, _, open| Ok((open, to_set_var.clone())),
                            set,
                        ),
                        _ => {
                            let name = call_name(fname);
                            let what = describe_non_lhs(assignee);
                            let example = match assignee {
                                Expr::Call(..) => format!(
                                    ": `s = {}; {}(re, repl, s)`",
                                    what.trim_matches('`'),
                                    name
                                ),
                                _ => String::new(),
                            };
                            let e = err_raw!(
                                "the 3rd argument of `{}` must be a variable, an array element or a field, not {}; assign it to a variable first{}",
                                name,
                                what,
                                example
                            );
                            Err(match self.loc_of(assignee) {
                                Some(loc) => e.with_loc(loc),
                                None => e,
                            })
                        }
                    }?;
                    self.f.cfg.add_edge(set_end, next, Transition::null());
                    return Ok((next, PrimExpr::Val(PrimVal::Var(res))));
                }

                if builtins::Function::GenSub == bi && args.len() == 3 {
                    // If a fourth argument isn't provided, we assume you mean $0.
                    let e = &Expr::Unop(ast::Unop::Column, &Expr::ILit(0));
                    let (next, v) = self.convert_val(e, open)?;
                    open = next;
                    prim_args.push(v);
                }
                // Defaults are filled in above: a call with the wrong number of arguments still
                // has it here. Checking it here (rather than when typing the call) locates the
                // error at the call.
                if let Some(arity) = bi.arity() {
                    if prim_args.len() != arity {
                        return Err(builtins::arity_error(&call_name(fname), args.len(), arity));
                    }
                }
                return Ok((open, PrimExpr::CallBuiltin(bi, prim_args)));
            }
        }
    }

    fn escape(&mut self, v: PrimVal<'b>, current_open: NodeIx) -> Result<PrimVal<'b>> {
        let builtin = match self.ctx.esc {
            Escaper::CSV => builtins::Function::EscapeCSV,

            Escaper::TSV => builtins::Function::EscapeTSV,
            Escaper::Identity => return Ok(v),
        };
        let e = PrimExpr::CallBuiltin(builtin, smallvec![v]);
        self.to_val(e, current_open)
    }

    fn fresh(&mut self) -> Ident {
        self.ctx.fresh()
    }

    fn fresh_local(&mut self) -> Ident {
        self.ctx.fresh_local()
    }

    fn record_ident(&mut self, id: Ident, blk: NodeIx) {
        record_ident(&mut self.f.defsites, &mut self.f.orig, id, blk);
    }

    fn get_identifier(&mut self, i: &I) -> Ident {
        // Look for any local variables with this name first, then search the global scope, then
        // create a fresh global variable.
        if let Some(ix) = self.f.args_map.get(i) {
            self.f.args[*ix as usize].id
        } else if let Some(id) = self.ctx.hm.get(i) {
            // We have found a global identifier that is not in main. Make sure it is not marked as
            // local.
            if id.global && !self.f.name.is_main() {
                self.ctx.local_globals.remove(&id.low);
            }
            *id
        } else {
            let next = self.fresh();
            self.ctx.hm.insert(i.clone(), next);
            self.ctx.may_rename.push(next);
            if self.f.name.is_main() {
                self.ctx.local_globals.insert(next.low);
            }
            next
        }
    }

    fn add_stmt(&mut self, at: NodeIx, stmt: PrimStmt<'b>) -> Result<()> {
        if let PrimStmt::AsgnVar(ident, _) = stmt {
            self.record_ident(ident, at);
        }
        let bb = self.f.cfg.node_weight_mut(at).unwrap();
        if bb.sealed {
            return err!(
                "appending to sealed basic block ({}). Last instr={:?}",
                at.index(),
                bb.q.back()
            );
        }
        bb.q.push_back(stmt);
        Ok(())
    }

    fn seal(&mut self, at: NodeIx) {
        self.f.cfg.node_weight_mut(at).unwrap().sealed = true;
    }

    /// The blocks that read each variable before assigning it (reads in a block's outgoing
    /// transitions come after all of its statements).
    fn upward_exposed_uses(&self) -> HashMap<Ident, Vec<NodeIx>> {
        let mut res = HashMap::<Ident, Vec<NodeIx>>::default();
        let mut defined = HashSet::<Ident>::default();
        for n in self.f.cfg.node_indices() {
            defined.clear();
            let mut add = |id: Ident, defined: &HashSet<Ident>| {
                if !defined.contains(&id) {
                    let blocks = res.entry(id).or_default();
                    if blocks.last() != Some(&n) {
                        blocks.push(n);
                    }
                }
            };
            for stmt in self.f.cfg.node_weight(n).unwrap().q.iter() {
                stmt.visit_uses(|id| add(id, &defined));
                if let PrimStmt::AsgnVar(id, _) = stmt {
                    defined.insert(*id);
                }
            }
            for e in self.f.cfg.edges_directed(n, Direction::Outgoing) {
                if let Transition(Some(PrimVal::Var(id))) = e.weight() {
                    add(*id, &defined);
                }
            }
        }
        res
    }

    // Phi nodes are only inserted where the variable is live (pruned SSA). Otherwise a temporary
    // (e.g. the result of `?:` or `&&`) gets a dead phi node at every enclosing join point, which
    // is quadratic in the length of a chain of `&&`.
    fn insert_phis(&mut self) {
        use crate::common::WorkList;
        // TODO: do we need defsites and orig after this, or can we deallocate them here?

        let uses = self.upward_exposed_uses();
        // live[n] == i + 1 when the i'th variable in may_rename is live on entry to block n.
        let mut live = vec![0usize; self.f.cfg.node_count()];
        let mut stack = Vec::new();
        // phis: the set of basic blocks that must have a phi node for a given variable.
        let mut phis = HashMap::<Ident, HashSet<NodeIx>>::new();
        let mut worklist = WorkList::default();
        // TODO rework this iteration to explicitly intersect may_rename and defsites? That way we
        // do O(min(|defsites|,|may_rename|)) work.
        for (i, ident) in self.ctx.may_rename.iter().cloned().enumerate() {
            if ident.global && self.ctx.local_globals.get(&ident.low).is_none() {
                continue;
            }
            // Add all defsites into the worklist.
            let defsites = if let Some(ds) = self.f.defsites.get(&ident) {
                ds
            } else {
                continue;
            };
            let use_blocks = match uses.get(&ident) {
                Some(u) => u,
                None => continue,
            };
            // Compute the blocks where `ident` is live on entry: walk backwards from its uses,
            // stopping at blocks that assign it.
            let stamp = i + 1;
            for n in use_blocks.iter().copied() {
                if live[n.index()] != stamp {
                    live[n.index()] = stamp;
                    stack.push(n);
                }
            }
            while let Some(n) = stack.pop() {
                for p in self.f.cfg.neighbors_directed(n, Direction::Incoming) {
                    if live[p.index()] != stamp && !defsites.contains(&p) {
                        live[p.index()] = stamp;
                        stack.push(p);
                    }
                }
            }
            worklist.extend(defsites.iter().copied());
            while let Some(node) = worklist.pop() {
                // For all nodes on the dominance frontier without phi nodes for this identifier,
                // create a phi node of the appropriate size and insert it at the front of the
                // block (no renaming).
                for d in self.f.df[node.index()].iter() {
                    let d_ix = NodeIx::new(*d as usize);
                    if live[d_ix.index()] != stamp {
                        continue;
                    }
                    if phis.get(&ident).map(|s| s.contains(&d_ix)) != Some(true) {
                        let phi = PrimExpr::Phi(
                            self.f
                                .cfg
                                .neighbors_directed(d_ix, Direction::Incoming)
                                .map(|n| (n, ident))
                                .collect(),
                        );
                        let stmt = PrimStmt::AsgnVar(ident, phi);
                        self.f
                            .cfg
                            .node_weight_mut(d_ix)
                            .expect("node in dominance frontier must be valid")
                            .q
                            .push_front(stmt);
                        phis.entry(ident).or_insert(HashSet::default()).insert(d_ix);
                        if !defsites.contains(&d_ix) {
                            worklist.insert(d_ix);
                        }
                    }
                }
            }
        }
    }

    fn rename(&mut self, cur: NodeIx) {
        #[derive(Clone)]
        struct RenameStack {
            // global variables do not get renamed and have no phi functions.
            global: bool,
            count: NumTy,
            stack: SmallVec<NumTy>,
        }

        impl RenameStack {
            fn latest(&self) -> NumTy {
                if self.global {
                    0
                } else {
                    *self
                        .stack
                        .last()
                        .expect("variable stack should never be empty")
                }
            }
            fn get_next(&mut self) -> NumTy {
                if self.global {
                    0
                } else {
                    let next = self.count + 1;
                    self.count = next;
                    self.stack.push(next);
                    next
                }
            }
            fn pop(&mut self) {
                if !self.global {
                    self.stack.pop().unwrap();
                }
            }
        }

        fn rename_recursive<I>(f: &mut Function<I>, cur: NodeIx, state: &mut Vec<RenameStack>) {
            // We need to remember which new variables are introduced in this frame so we can
            // remove them when we are done.
            let mut defs = smallvec::SmallVec::<[NumTy; 16]>::new();

            // First, go through all the statements and update the variables to the highest
            // subscript (second component in Ident).
            for stmt in &mut f
                .cfg
                .node_weight_mut(cur)
                .expect("rename must be passed valid node indices")
                .q
            {
                // Note that `replace` is specialized to our use-case in this method. It does not hit
                // AsgnVar identifiers, and it skips Phi nodes.
                stmt.replace(|Ident { low, global, .. }| Ident {
                    low,
                    sub: state[low as usize].latest(),
                    global,
                });
                if let PrimStmt::AsgnVar(Ident { low, sub, .. }, _) = stmt {
                    *sub = state[*low as usize].get_next();
                    defs.push(*low);
                }
            }

            // The recursion is structured around the dominator tree. That means that normal
            // renaming may not update join points in a graph to the right value. Consider
            //
            //            A (x=1)
            //          /   \
            //         /     \
            //        B (x++) C (x+=2)
            //         \     /
            //          \   /
            //            D (x+=5)
            //
            // With flow pointing downward. The dominator tree looks like
            //
            //          A
            //        / | \
            //       B  C  D
            //
            // Without this while loop, we would wind up with something like:
            // A: x0 = 1
            // B: x1 = x0 + 1
            // C: x2 = x0 + 2
            // D: x3 = phi(x0, x0); x4 = x3 + 5
            //
            // But of course the phi node is wrong, it should be:
            // x3 = phi(x1, x2)
            //
            // To fix this, we iterate over any outgoing neighbors and find phi functions that
            // point back to the current node and update the subscript accordingly.
            let mut walker = f.cfg.neighbors_directed(cur, Direction::Outgoing).detach();
            while let Some((edge, neigh)) = walker.next(&f.cfg) {
                if let Some(PrimVal::Var(Ident { low, sub, .. })) =
                    &mut f.cfg.edge_weight_mut(edge).unwrap().0
                {
                    *sub = state[*low as usize].latest();
                }
                for stmt in &mut f.cfg.node_weight_mut(neigh).unwrap().q {
                    if let PrimStmt::AsgnVar(_, PrimExpr::Phi(ps)) = stmt {
                        for (pred, Ident { low, sub, .. }) in ps.iter_mut() {
                            if pred == &cur {
                                *sub = state[*low as usize].latest();
                                break;
                            }
                        }
                    }
                }
            }
            for child in f.dt[cur.index()].clone().iter() {
                rename_recursive(f, NodeIx::new(*child as usize), state);
            }
            for d in defs.into_iter() {
                state[d as usize].pop();
            }
        }
        let mut state = vec![
            RenameStack {
                global: false,
                count: 0,
                stack: smallvec![0],
            };
            self.ctx.max as usize
        ];
        for id in self.ctx.hm.values() {
            if id.global && self.ctx.local_globals.get(&id.low).is_none() {
                state[id.low as usize].global = true;
            }
        }
        rename_recursive(self.f, cur, &mut state);
    }
}
