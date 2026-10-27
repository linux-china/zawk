/// An "Abstract Syntax Tree" that fairly closely resembles the structure of an AWK program. This
/// is the representation that the parser returns. A couple of basic desugaring rules are applied
/// that translate a `Prog` into a bare `Stmt`, along with its accompanying function definitions.
/// Those materials are consumed by the `cfg` module, which produces an untyped SSA form.
///
/// A couple things to note:
///  * The Expr and Stmt types are trees supporting arbitrary nesting. With limited exceptions, we
///    do not allocate each node separately on the heap. We instead use an arena for these
///    allocations. This is a win on all fronts: it's strictly faster because allocation is
///    extremely cheap, and destructors are fairly cheap, _and_ it's much easier to program because
///    references are Copy.
///  * The common way of introducing AWK: that it is a language structured around patterns and
///    actions to execute when the input matches that pattern is desugared in this module. We do
///    not handle it specially.
///
///    TODO It is not clear that this is the right move long-term: lots of regex implementations
///    (like HyperScan, or BurntSushi's engine in use here) achieve higher throughput by matching a
///    string against several patterns at once. There is probably a transformation we could do here
///    to take advantage of that, but it would probably involve building out def-use chains (which
///    we currently don't do), and we'd want to verify that performance didn't degrade when the
///    patterns are _not sparse_ in the input.
use crate::arena::{self, Arena};
use crate::builtins::Function;
use crate::common::{Either, FileSpec, Stage};
use crate::lexer::Loc;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Unop {
    Column,
    Not,
    Neg,
    Pos,
}

#[derive(Debug)]
pub struct FunDec<'a, 'b, I> {
    pub name: I,
    /// The parameters, followed by the `local` variables of the body (see `LocalScope`).
    pub args: Vec<I>,
    /// The number of declared parameters at the start of `args`.
    pub params: usize,
    pub body: &'a Stmt<'a, 'b, I>,
}

#[derive(Debug)]
pub enum Pattern<'a, 'b, I> {
    Null,
    Bool(&'a Expr<'a, 'b, I>),
    Comma(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
}

pub struct Prog<'a, 'b, I> {
    // We allocate as much from the arena as we can, except for things that will be allocated as
    // vectors anyway.

    // FS
    pub field_sep: Option<&'b [u8]>,
    pub prelude_vardecs: Vec<(I, &'a Expr<'a, 'b, I>)>,
    // OFS
    pub output_sep: Option<&'b [u8]>,
    // ORS
    pub output_record_sep: Option<&'b [u8]>,
    // function declares
    pub decs: arena::Vec<'a, FunDec<'a, 'b, I>>,
    // BEGIN blocks
    pub begin: arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
    // PREPARE blocks, executed after the main loop
    pub prepare: arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
    // BEGINFILE
    pub beginfile: arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
    // ENDFILE
    pub endfile: arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
    // END blocks
    pub end: arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
    // pattern actions
    pub pats: arena::Vec<'a, (Pattern<'a, 'b, I>, Option<&'a Stmt<'a, 'b, I>>)>,
    pub stage: Stage<()>,
    pub argv: Vec<&'b str>,
    // Command-line `var=value` operands: `operand_assigns[k]` are the assignments before the k-th
    // input file in `operand_files`, and the last entry those after the last file. Unlike `-v`,
    // they run when the main loop reaches that file (or before END). Empty without such operands.
    pub operand_assigns: Vec<Vec<(I, &'a Expr<'a, 'b, I>)>>,
    pub operand_files: Vec<&'b str>,
    pub parse_header: bool,
    // Each input file starts with its own header line (-H, but not for JSON Lines or Parquet,
    // whose FI comes from the record keys).
    pub header_per_file: bool,
    // The header of every input file, including the first one, is read by the main loop rather
    // than in BEGIN: the input files are NATS messages, which BEGIN must not wait for.
    pub header_in_main: bool,
    // Where calls and variable references start in the program text, for error messages: see
    // `expr_key`.
    pub locs: ExprLocs,
    // The `local` variables of the rule or function being parsed.
    pub locals: LocalScope<I>,
}

/// The value of a variable that is never assigned: `local x` assigns `%uninit@N` (its own copy,
/// see `uninit_var`) to `x@N`, which makes `x` uninitialized again (see `parsing::declare_local`).
pub const UNINIT_VAR: &str = "%uninit";

/// The never-assigned variable that `local` assigns to the local variable `name` (`x@N`). Each
/// local has its own, so that a scalar local used as an array is the only one that conflicts.
pub fn uninit_var(name: &str) -> String {
    let n = name.rsplit_once('@').map_or("", |(_, n)| n);
    format!("{}@{}", UNINIT_VAR, n)
}

/// Whether `name` is a variable made by `uninit_var`.
pub fn is_uninit_var(name: &str) -> bool {
    source_name(name) == UNINIT_VAR
}

/// The name of the variable `name` in the program: the name of a `local` variable without the
/// suffix that makes it unique (see `LocalScope`).
pub fn source_name(name: &str) -> &str {
    match name.rsplit_once('@') {
        Some((base, n)) if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => name,
    }
}

/// `text` with the names of `local` variables quoted in it (`` `x@1` ``) replaced by their names in
/// the program (see `source_name`).
pub fn source_names(text: &str) -> String {
    text.split('`')
        .enumerate()
        .map(|(i, s)| if i % 2 == 1 { source_name(s) } else { s })
        .collect::<Vec<_>>()
        .join("`")
}

/// Whether `name` is the new name of a `local` variable.
pub fn is_local_name(name: &str) -> bool {
    source_name(name).len() != name.len()
}

/// The `local` variables declared so far in the rule or function being parsed.
///
/// A local variable `x` is renamed to `x@N`, which can not be written in a program, from its
/// declaration to the end of the rule or function, so the rest of the compiler sees an ordinary
/// variable: in a rule it is a global only referenced from the main program, which is kept in a
/// register, and in a function it is an extra parameter. A local in a rule is uninitialized on
/// each run of the rule: its declaration assigns it `uninit_var` (or `delete`s an array), and the
/// rule starts with such an assignment for the locals declared in nested statements.
pub struct LocalScope<I> {
    /// The new names of the visible locals.
    pub names: hashbrown::HashMap<I, I>,
    /// The locals declared in the rule or function: the new name, whether it is an array, and the
    /// key (`expr_key`) of its declaration statement.
    pub decls: Vec<(I, bool, usize)>,
    /// The number of locals declared in the program, which numbers their new names.
    pub count: usize,
    /// The names of the locals declared in the program, and where, to check that none is named
    /// after a function (see `parsing::check_local_names`).
    pub declared: Vec<(I, Loc)>,
}

impl<I> Default for LocalScope<I> {
    fn default() -> Self {
        LocalScope {
            names: Default::default(),
            decls: Vec::new(),
            count: 0,
            declared: Vec::new(),
        }
    }
}

/// Locations of expressions in the program text, keyed by `expr_key`.
pub type ExprLocs = hashbrown::HashMap<usize, Loc>;

/// The key of an expression in `ExprLocs`: its address. AST nodes are allocated in an arena, so
/// the address of a node is stable and unique while the program is compiled.
pub fn expr_key<T>(e: &T) -> usize {
    e as *const T as usize
}

// Pick an illegal frawk identifier.
const LOOP_VAR: &str = "--";

// Statements that fill FI from the column names in the current record:
//  for (LOOP_VAR=1; LOOP_VAR <= NF; ++LOOP_VAR)
//      FI[$LOOP_VAR] = LOOP_VAR;
//  update_used_fields()
fn set_fi_from_record<'a, 'b, I: From<&'b str> + Clone>(
    arena: &'a Arena,
) -> arena::Vec<'a, &'a Stmt<'a, 'b, I>> {
    use {self::Expr::*, Stmt::*};
    let loop_var = arena.alloc(Var(LOOP_VAR.into()));
    let init = arena.alloc(Expr(arena.alloc(Assign(loop_var, arena.alloc(ILit(1))))));
    let cond = arena.alloc(Binop(
        self::Binop::LTE,
        loop_var,
        arena.alloc(Var("NF".into())),
    ));
    let update = arena.alloc(Expr(arena.alloc(Inc {
        is_inc: true,
        is_post: false,
        x: loop_var,
    })));
    let body = arena.alloc(Expr(arena.alloc(Call(
        Either::Right(Function::SetFI),
        arena.alloc_slice(&[loop_var, loop_var]),
    ))));

    arena.new_vec_from_slice(&[
        arena.alloc(For(Some(init), Some(cond), Some(update), body)),
        arena.alloc(Expr(
            arena.alloc(Call(Either::Right(Function::UpdateUsedFields), &[])),
        )),
    ])
}

fn parse_header<'a, 'b, I: From<&'b str> + Clone>(
    arena: &'a Arena,
    begin: &mut arena::Vec<'a, &'a Stmt<'a, 'b, I>>,
) {
    use {self::Expr::*, Stmt::*};
    // Append the following to begin:
    // if (getline > 0) {
    //  <set_fi_from_record>
    // }
    begin.push(arena.alloc(If(
        arena.alloc(Binop(
            self::Binop::GT,
            arena.alloc(ReadStdin),
            arena.alloc(ILit(0)),
        )),
        arena.alloc(Block(set_fi_from_record(arena))),
        /*else*/ None,
    )));
}

// With -H, every input file starts with a header line, not just the first one. The header of
// the first file is read after BEGIN (see `parse_header`); the main loop consumes the header of each
// following file and rebuilds FI from it, as the columns may be in a different order.
//
// FNR is 0 for the first record of a file. To tell the header of a new file apart from the first
// record after a header, FNR is set to -1 once a header has been read:
//  if (FNR <= 0) {
//      if (FNR == 0) {
//          delete FI
//          <set_fi_from_record>
//          FNR = -1
//          next
//      }
//      FNR = 0
//  }
fn header_per_file<'a, 'b, I: From<&'b str> + Clone>(arena: &'a Arena) -> &'a Stmt<'a, 'b, I> {
    use {self::Binop::*, self::Expr::*, Stmt::*};
    let fnr = arena.alloc(Var("FNR".into()));
    let set_fnr = |v| &*arena.alloc(Expr(arena.alloc(Assign(fnr, arena.alloc(ILit(v))))));
    let mut header = arena.vec_with_capacity(5);
    header.push(&*arena.alloc(Expr(arena.alloc(Call(
        Either::Right(Function::Clear),
        arena.alloc_slice(&[&*arena.alloc(Var("FI".into()))]),
    )))));
    header.extend(set_fi_from_record(arena).iter().cloned());
    header.push(set_fnr(-1));
    header.push(arena.alloc(Next));
    let new_file = arena.alloc(If(
        arena.alloc(Binop(EQ, fnr, arena.alloc(ILit(0)))),
        arena.alloc(Block(header)),
        None,
    ));
    arena.alloc(If(
        arena.alloc(Binop(LTE, fnr, arena.alloc(ILit(0)))),
        arena.alloc(Block(arena.new_vec_from_slice(&[&*new_file, set_fnr(0)]))),
        None,
    ))
}

// Hidden variables (illegal identifiers) tracking command-line `var=value` operands: the index of
// the next group of assignments, the file being read, and whether that file has been reached.
const OPERAND_IDX: &str = "--operand-idx";
const OPERAND_FILE: &str = "--operand-file";
const OPERAND_DONE: &str = "--operand-done";

// The assignments of group `k` of operands, followed by OPERAND_IDX = k + 1.
fn operand_group<'a, 'b, I: From<&'b str> + Clone>(
    arena: &'a Arena,
    k: usize,
    group: &[(I, &'a Expr<'a, 'b, I>)],
) -> arena::Vec<'a, &'a Stmt<'a, 'b, I>> {
    use {self::Expr::*, Stmt::*};
    let mut block = arena.vec_with_capacity(group.len() + 2);
    for (ident, exp) in group.iter() {
        block.push(&*arena.alloc(Expr(
            arena.alloc(Assign(arena.alloc(Var(ident.clone())), exp)),
        )));
    }
    block.push(arena.alloc(Expr(arena.alloc(Assign(
        arena.alloc(Var(OPERAND_IDX.into())),
        arena.alloc(ILit(k as i64 + 1)),
    )))));
    block
}

// Runs the `var=value` operands before the current input file when a new file starts, i.e. the
// groups of assignments up to that file's operand. A file that produced no records (empty,
// skipped) has its preceding assignments run with those of the next file:
//  if (FNR == 0 || FILENAME != OPERAND_FILE) {
//      OPERAND_FILE = FILENAME
//      OPERAND_DONE = 0
//      if (!OPERAND_DONE && OPERAND_IDX == k) {   // for each file operand k
//          <group k>
//          if (FILENAME == <file k>) OPERAND_DONE = 1
//      }
//  }
fn operand_assigns_new_file<'a, 'b, I: From<&'b str> + Clone + PartialEq>(
    arena: &'a Arena,
    groups: &[Vec<(I, &'a Expr<'a, 'b, I>)>],
    files: &[&'b str],
) -> &'a Stmt<'a, 'b, I> {
    use {self::Binop::*, self::Expr::*, Stmt::*};
    let idx = arena.alloc(Var(OPERAND_IDX.into()));
    let file = arena.alloc(Var(OPERAND_FILE.into()));
    let done = arena.alloc(Var(OPERAND_DONE.into()));
    let filename = arena.alloc(Var("FILENAME".into()));
    let fnr = arena.alloc(Var("FNR".into()));
    let mut body = arena.vec_with_capacity(files.len() + 2);
    body.push(&*arena.alloc(Expr(arena.alloc(Assign(file, filename)))));
    body.push(arena.alloc(Expr(arena.alloc(Assign(done, arena.alloc(ILit(0)))))));
    // Without file operands, standard input is read after all the assignments.
    for k in 0..files.len().max(1) {
        let mut block = operand_group(arena, k, &groups[k]);
        // The record was read before the assignments: split it again with a new FS.
        if groups[k].iter().any(|(ident, _)| *ident == I::from("FS")) {
            let record = arena.alloc(Unop(self::Unop::Column, arena.alloc(ILit(0))));
            block.push(arena.alloc(Expr(arena.alloc(Assign(record, record)))));
        }
        if let Some(f) = files.get(k) {
            block.push(arena.alloc(If(
                arena.alloc(Binop(EQ, filename, arena.alloc(StrLit(f.as_bytes())))),
                arena.alloc(Expr(arena.alloc(Assign(done, arena.alloc(ILit(1)))))),
                None,
            )));
        }
        body.push(arena.alloc(If(
            arena.alloc(And(
                arena.alloc(Unop(self::Unop::Not, done)),
                arena.alloc(Binop(EQ, idx, arena.alloc(ILit(k as i64)))),
            )),
            arena.alloc(Block(block)),
            None,
        )));
    }
    arena.alloc(If(
        arena.alloc(Or(
            arena.alloc(Binop(EQ, fnr, arena.alloc(ILit(0)))),
            arena.alloc(Unop(
                self::Unop::Not,
                arena.alloc(Binop(EQ, filename, file)),
            )),
        )),
        arena.alloc(Block(body)),
        None,
    ))
}

// Runs the `var=value` operands not run yet (those after the last file, or before files that
// produced no records) before END:
//  if (OPERAND_IDX == k) <group k>   // for each group k
fn operand_assigns_end<'a, 'b, I: From<&'b str> + Clone>(
    arena: &'a Arena,
    groups: &[Vec<(I, &'a Expr<'a, 'b, I>)>],
) -> arena::Vec<'a, &'a Stmt<'a, 'b, I>> {
    use {self::Binop::*, self::Expr::*, Stmt::*};
    let idx = arena.alloc(Var(OPERAND_IDX.into()));
    let mut res = arena.vec_with_capacity(groups.len());
    for (k, group) in groups.iter().enumerate() {
        res.push(&*arena.alloc(If(
            arena.alloc(Binop(EQ, idx, arena.alloc(ILit(k as i64)))),
            arena.alloc(Block(operand_group(arena, k, group))),
            None,
        )));
    }
    res
}

impl<'a, 'b, I: From<&'b str> + Clone> Prog<'a, 'b, I> {
    pub(crate) fn from_stage(arena: &'a Arena, stage: Stage<()>) -> Self {
        Prog {
            field_sep: None,
            prelude_vardecs: Vec::new(),
            output_sep: None,
            output_record_sep: None,
            decs: arena.new_vec(),
            begin: arena.new_vec(),
            prepare: arena.new_vec(),
            beginfile: arena.new_vec(),
            endfile: arena.new_vec(),
            end: arena.new_vec(),
            pats: arena.new_vec(),
            argv: Vec::new(),
            operand_assigns: Vec::new(),
            operand_files: Vec::new(),
            parse_header: false,
            header_per_file: false,
            header_in_main: false,
            stage,
            locs: Default::default(),
            locals: Default::default(),
        }
    }
    pub(crate) fn desugar_stage(&self, arena: &'a Arena) -> Stage<&'a Stmt<'a, 'b, I>>
    where
        I: PartialEq,
    {
        use {self::Binop::*, self::Expr::*, Stmt::*};
        let mut conds = 0;

        let mut begin = arena.vec_with_capacity(self.begin.len() * 2);
        let mut main_loop = None;
        let mut end = None;

        // Desugar -F flag
        if let Some(sep) = self.field_sep {
            begin.push(arena.alloc(Expr(arena.alloc(Assign(
                arena.alloc(Var("FS".into())),
                arena.alloc(StrLit(sep)),
            )))));
        }

        // Support "output csv/tsv" mode
        if let Some(sep) = self.output_sep {
            begin.push(arena.alloc(Expr(arena.alloc(Assign(
                arena.alloc(Var("OFS".into())),
                arena.alloc(StrLit(sep)),
            )))));
        }
        if let Some(sep) = self.output_record_sep {
            begin.push(arena.alloc(Expr(arena.alloc(Assign(
                arena.alloc(Var("ORS".into())),
                arena.alloc(StrLit(sep)),
            )))));
        }

        // Assign SUBSEP, which we treat as a normal variable
        begin.push(arena.alloc(Expr(arena.alloc(Assign(
            arena.alloc(Var("SUBSEP".into())),
            arena.alloc(StrLit(&[0o034u8])),
        )))));
        // Desugar -v flags
        for (ident, exp) in self.prelude_vardecs.iter() {
            begin.push(arena.alloc(Expr(
                arena.alloc(Assign(arena.alloc(Var(ident.clone())), exp)),
            )));
        }

        // Set argc, argv
        if !self.argv.is_empty() {
            begin.push(arena.alloc(Expr(arena.alloc(Assign(
                arena.alloc(Var("ARGC".into())),
                arena.alloc(ILit(self.argv.len() as i64)),
            )))));
            let argv = arena.alloc(Var("ARGV".into()));
            for (ix, arg) in self.argv.iter().enumerate() {
                let arg = arena.alloc(StrLit(arg.as_bytes()));
                let ix = arena.alloc(ILit(ix as i64));
                let arr_exp = arena.alloc(Index(argv, ix));
                begin.push(arena.alloc(Expr(arena.alloc(Assign(arr_exp, arg)))));
            }
        }

        begin.extend(self.begin.iter().cloned());

        // Desugar patterns into if statements, with the usual desugaring for an empty action.
        let mut inner = arena.vec_with_capacity(10);
        let per_file_header = self.parse_header
            && self.header_per_file
            && matches!(self.stage, Stage::Main(_));
        let has_operand_assigns = !self.operand_assigns.is_empty();
        if has_operand_assigns {
            inner.push(operand_assigns_new_file(
                arena,
                &self.operand_assigns,
                &self.operand_files,
            ));
        }
        if per_file_header {
            inner.push(header_per_file(arena));
        }
        inner.push(arena.alloc(Expr(arena.alloc(Inc {
            is_inc: true,
            is_post: false,
            x: arena.alloc(Var("NR".into())),
        }))));
        inner.push(arena.alloc(Expr(arena.alloc(Inc {
            is_inc: true,
            is_post: false,
            x: arena.alloc(Var("FNR".into())),
        }))));
        let init_len = inner.len();
        for (pat, body) in self.pats.iter() {
            let body = if let Some(body) = body {
                body
            } else {
                arena.alloc(Print(&[], None))
            };
            match pat {
                Pattern::Null => inner.push(body),
                Pattern::Bool(pat) => inner.push(arena.alloc(If(pat, body, None))),
                Pattern::Comma(l, r) => {
                    let mut block = arena.vec_with_capacity(2);
                    // Comma patterns run the corresponding action between pairs of lines matching
                    // patterns `l` and `r`, inclusive. One common example is the patterh
                    //  /\/*/,/*\//
                    // Matching block comments in several propular languages. We desugar these with
                    // special statements StartCond, LastCond, EndCond, as well as the Cond
                    // expression. Each of these is tagged with an identifier indicating which
                    // comma pattern the statement or expression is referencing. In the cfg module,
                    // these are compiled to simple assignments and reads on a pattern-specific
                    // local variable:
                    //
                    // StartCond sets the variable to 1
                    // EndCond sets the variable to 0
                    // LastCond stes the variable to 2
                    // Cond reads the variable.
                    //
                    // Why do you need LastCond? We can mostly make due without it. Consider the
                    // fragment:
                    //   /\/*/,/*\// { i++ }
                    // We can desugar this quite easily as:
                    //   /\/*/ { StartCond(0); } # _cond_0 = 1
                    //   Cond(0) { i++; }        # if _cond_0
                    //   /*\// { EndCond(0); }   # _cond_0 = 0
                    // We get into trouble, however, if control flow is more complex. If we wanted
                    // to strip comments we might write:
                    //   /\/*/,/*\// { next; }
                    //   { print; }
                    // But applying the above desugaring rules leads us to running `next` before we
                    // can set EndCond. No output will be printed after we encounter our first
                    // comment, regardless of what comes after.
                    //
                    // To fix this, we introduce LastCond and use it to signal that the pattern
                    // should not match in the next iteration.
                    //   /\/*/ { StartCond(0); } # _cond_0 = 1
                    //   /*\// { LastCond(0); }  # _cond_0 = 2
                    //   Cond(0) {
                    //      # We matched the end of a comment. End the condition before `next`;
                    //      if (Cond(0) == 2) EndCond(0); # _cond_0 = 0;
                    //      next;
                    //  }
                    //
                    // As in POSIX awk and gawk, the start pattern is only evaluated outside of the
                    // range and the end pattern only inside of it (including the line that starts
                    // it), so a line matching only the end pattern does not run the action:
                    //   if (Cond(0) == 0) { if (/\/*/) StartCond(0); }
                    //   if (Cond(0) == 1) { if (/*\//) LastCond(0); }
                    let cond_is = |v| arena.alloc(Binop(EQ, arena.alloc(Cond(conds)), arena.alloc(ILit(v))));
                    let start = arena.alloc(If(l, arena.alloc(StartCond(conds)), None));
                    inner.push(arena.alloc(If(cond_is(0), start, None)));
                    let last = arena.alloc(If(r, arena.alloc(LastCond(conds)), None));
                    inner.push(arena.alloc(If(cond_is(1), last, None)));
                    block.push(arena.alloc(If(
                        arena.alloc(Binop(EQ, arena.alloc(Cond(conds)), arena.alloc(ILit(2)))),
                        arena.alloc(EndCond(conds)),
                        None,
                    )));
                    block.push(body);
                    inner.push(arena.alloc(If(
                        arena.alloc(Cond(conds)),
                        arena.alloc(Block(block)),
                        None,
                    )));
                    conds += 1;
                }
            }
        }

        if !self.end.is_empty() || !self.prepare.is_empty() || inner.len() > init_len {
            // Wrap the whole thing in a while((getline) > 0) { } statement.
            let main_portion = arena.alloc(While(
                /*is_toplevel=*/ true,
                arena.alloc(Binop(GT, arena.alloc(ReadStdin), arena.alloc(ILit(0)))),
                arena.alloc(Block(inner)),
            ));
            let main_portion = if per_file_header {
                // FNR = -1: the header of the first file was read after BEGIN. After the loop, a
                // trailing header (a file without records) must not leave FNR at -1 for END.
                let fnr = arena.alloc(Var("FNR".into()));
                let set_fnr = |v| &*arena.alloc(Expr(arena.alloc(Assign(fnr, arena.alloc(ILit(v))))));
                arena.alloc(Block(arena.new_vec_from_slice(&[
                    set_fnr(-1),
                    main_portion,
                    arena.alloc(If(
                        arena.alloc(Binop(LT, fnr, arena.alloc(ILit(0)))),
                        set_fnr(0),
                        None,
                    )),
                ])))
            } else {
                main_portion
            };
            main_loop = Some(if self.prepare.is_empty() {
                main_portion
            } else {
                let mut block = arena.vec_with_capacity(self.prepare.len() + 1);
                block.push(main_portion);
                block.extend(self.prepare.iter().cloned());
                arena.alloc(Block(block))
            });
        }
        // for -H: the header is read after BEGIN, so that FS, ARGV etc. set by `-v` or BEGIN apply to
        // it, and only when input is read at all: a BEGIN-only program does not read its input.
        if self.parse_header && !self.header_in_main && (main_loop.is_some() || !self.end.is_empty()) {
            parse_header(arena, &mut begin);
        }
        if !self.end.is_empty() {
            let end_block = if has_operand_assigns {
                let mut block = operand_assigns_end(arena, &self.operand_assigns);
                block.extend(self.end.iter().cloned());
                block
            } else {
                self.end.clone()
            };
            end = Some(arena.alloc(Block(end_block)));
        }
        match self.stage {
            Stage::Main(_) => {
                let end = end.map(|end| &*arena.alloc(EndBlock(end)));
                begin.extend(main_loop.into_iter().chain(end));
                Stage::Main(arena.alloc(Block(begin)))
            }
            Stage::Par { .. } => Stage::Par {
                begin: if begin.is_empty() {
                    None
                } else {
                    Some(arena.alloc(Block(begin)))
                },
                main_loop,
                end,
            },
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Binop {
    Plus,
    Minus,
    Mult,
    Div,
    Mod,
    Concat,
    IsMatch,
    Pow,
    LT,
    GT,
    LTE,
    GTE,
    EQ,
}

#[derive(Debug, Clone)]
pub enum Expr<'a, 'b, I> {
    ILit(i64),
    FLit(f64),
    StrLit(&'b [u8]),
    PatLit(&'b [u8]),
    Unop(Unop, &'a Expr<'a, 'b, I>),
    Binop(Binop, &'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    Call(Either<I, Function>, &'a [&'a Expr<'a, 'b, I>]),
    Var(I),
    Index(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    Assign(
        &'a Expr<'a, 'b, I>, /*var or index expression*/
        &'a Expr<'a, 'b, I>,
    ),
    AssignOp(&'a Expr<'a, 'b, I>, Binop, &'a Expr<'a, 'b, I>),
    And(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    Or(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    /// `l ?? r`: `l` if it is true as a strnum (not uninitialized, empty, or a zero such as `"0.0"`),
    /// `r` otherwise.
    Nullish(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    /// `try e else f`: `ERRNO` is cleared and `e` evaluated, with stdlib warnings not printed.
    /// If nothing failed in `e` (failures an inner `try` handled don't count, see
    /// `runtime::try_scope`), the result is `e` and `ERRNO` gets back its previous value;
    /// otherwise the result is `f`, which may read `ERRNO`.
    Try(&'a Expr<'a, 'b, I>, &'a Expr<'a, 'b, I>),
    ITE(
        &'a Expr<'a, 'b, I>,
        &'a Expr<'a, 'b, I>,
        &'a Expr<'a, 'b, I>,
    ),
    Inc {
        is_inc: bool,
        is_post: bool,
        x: &'a Expr<'a, 'b, I>,
    },
    Getline {
        into: Option<&'a Expr<'a, 'b, I>>,
        from: Option<&'a Expr<'a, 'b, I>>,
        is_file: bool,
    },
    ReadStdin,
    // Used for comma patterns
    Cond(usize),
}

#[derive(Debug, Clone)]
pub enum Stmt<'a, 'b, I> {
    StartCond(usize),
    EndCond(usize),
    LastCond(usize),
    Expr(&'a Expr<'a, 'b, I>),
    Block(arena::Vec<'a, &'a Stmt<'a, 'b, I>>),
    Print(
        &'a [&'a Expr<'a, 'b, I>],
        Option<(&'a Expr<'a, 'b, I>, FileSpec)>,
    ),
    // Unlike print, printf must have at least one argument.
    Printf(
        &'a Expr<'a, 'b, I>,
        &'a [&'a Expr<'a, 'b, I>],
        Option<(&'a Expr<'a, 'b, I>, FileSpec)>,
    ),
    If(
        &'a Expr<'a, 'b, I>,
        &'a Stmt<'a, 'b, I>,
        Option<&'a Stmt<'a, 'b, I>>,
    ),
    For(
        Option<&'a Stmt<'a, 'b, I>>,
        Option<&'a Expr<'a, 'b, I>>,
        Option<&'a Stmt<'a, 'b, I>>,
        &'a Stmt<'a, 'b, I>,
    ),
    DoWhile(&'a Expr<'a, 'b, I>, &'a Stmt<'a, 'b, I>),
    // We mark some while loops as "special" because of the special "next" and "nextfile" commands,
    // that work as a special "labelled continue" for the toplevel loop.
    While(
        /*is_toplevel*/ bool,
        &'a Expr<'a, 'b, I>,
        &'a Stmt<'a, 'b, I>,
    ),
    ForEach(I, &'a Expr<'a, 'b, I>, &'a Stmt<'a, 'b, I>),
    // `for (i in start..end) body` (`start..=end` when inclusive): `i` takes the integers from
    // `int(start)` up to `int(end)`, both evaluated once before the loop.
    ForRange(
        I,
        &'a Expr<'a, 'b, I>,
        &'a Expr<'a, 'b, I>,
        /*inclusive*/ bool,
        &'a Stmt<'a, 'b, I>,
    ),
    // gawk's `switch (e) { case v: ... default: ... }`. Each case has a label (a numeric, string
    // or regex literal, or `None` for `default`) and a body; bodies fall through to the next case
    // unless they `break`.
    Switch(
        &'a Expr<'a, 'b, I>,
        &'a [(Option<&'a Expr<'a, 'b, I>>, &'a Stmt<'a, 'b, I>)],
    ),
    Break,
    Continue,
    Next,
    NextFile,
    Return(Option<&'a Expr<'a, 'b, I>>),
    // Exits the program at once with the given code, without running END: a failed `assert`.
    Abort(&'a Expr<'a, 'b, I>),
    // The END block of a serial program: `exit` from BEGIN or the main loop jumps here, see
    // `cfg::View::do_exit`.
    EndBlock(&'a Stmt<'a, 'b, I>),
}
