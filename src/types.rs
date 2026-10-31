//! Type inference for programs in untyped SSA form (i.e. the output of the cfg module).
//!
//! This is one of the stranger parts of frawk. If anyone has a pointer to a language that does
//! inference in this sort of way, I'd be very interested to read it!
//!
//! # Introduction
//! One somewhat interesting aspect of frawk is that it assigns static types to its variables. It
//! does this without breaking most of AWK's semantics --- the edge cases that do break could be
//! fixed fixed dynamically if need be; but for now they are breaking changes. In order to operate
//! on static types in modules "downstream" of the cfg module, we need to infer appropriate types
//! for our variables.
//!
//! This algorithm is a bit different from the standard algorithms for inferring types. Like the
//! classic [Hindley-Milner] setting, we want an algorithm that can assign types to variables and
//! functions without any annotations from the programmer. Our job is easier than this classic
//! setting in some ways, and harder in others.
//!
//! Unlike an ML-like language, we don't have all that many types. The interpreter really only
//! handles 11 or so types, and two of those (map iterators with integer and string key types) are
//! used in quite a limited way. This means that the types we infer are altogether simpler than
//! ones that are inferred in a language like Rust, OCaml or Haskell.
//!
//! # Motivation
//! Now onto the stuff that is harder. We can start by illustrating the types we assign in some
//! cases where it's not obvious how to proceed using a standard static type system as a model.
//!
//! ## One Variable, Many Types
//! However, some of these things are also harder. AWK variables can have multiple types over the
//! course of a program. It's quite easy to write:
//!
//! ```text
//! x = 1         : Int
//! x = "hello"   : String??
//! ```
//!
//! An easy way to implement this would be to interpret all variables as a big sum type, and
//! changing the dynamic type of a variable as it progresses through the program. We do not do
//! this, instead we convert the program to SSA form, where most instances of this pattern become
//!
//! ```text
//! x_0 = 1       : Int
//! x_1 = "hello" : String
//! ```
//!
//! Thereby keeping each variable to a single type. For variables that we cannot move into SSA form
//! (global variables, map keys), we insert coercions. Were `x` a global variable the example would
//! become:
//!
//! ```text
//! x = int-to-str(1)  : String
//! x = "hello"        : String
//! ```
//!
//! *Digression*: This is probably slower than a fully dynamic implementation, but the hypothesis when
//! implementing this was that global variables that are assigned multiple types are fairly rare.
//! If that turns out not to be the case, we could make our implementation of strings closer to the
//! sum type alluded to above.
//!
//! ## Types of Operations
//! We would like to distinguish between integers and floating-point numbers. But this can make it
//! tricky to interpret the types of even very simple expressions. For example, if we just consider
//! the statement:
//!
//! ```text
//! a = b + c
//! ```
//!
//! The type of `a` will be a `Float` if either `b` or `c` are floats; otherwise it will be an
//! integer. That doesn't sound too disturbing, but what if the whole program is something like
//! (supposing for the moment that `b` and `c` are global):
//!
//! ```text
//! a = b + c
//! b = a + c
//! c = a
//! ```
//!
//! This is all perfectly valid; like AWK, frawk does not require variables to be declared before
//! they are used. But it is a little strange: the types of `a`, `b` and `c` all depend on the
//! types of the other two variables. Were we to "untie the knot" by stipulating that any one of
//! them was an integer or floating point value, the other two variables would take on the same
//! type. The algorithm in this module makes a "judgment call" that we should prefer integers here.
//! More importantly, it handles this caes of recursive dependencies fairly gracefully.
//!
//! # The Basic Idea
//!
//! HM-style type inference is usually implemented using a form of unification. I'm no expert, but
//! it seems like while unification could be used for inserting coercions like in the first
//! example, it's a lot messier to get working when the types of functions' return types can depend
//! on the types of the arguments. At the very least, in my experience with languages like Haskell,
//! extensions like type families often require additional manual annotations.
//!
//! This module infers types based on a (hyper)graph, where nodes indicate our current information
//! about the types of a variable and edges specify the relationship between the types in one node
//! and another. Each node except for those representing constants starts off with no information
//! about its type, but as new information arrives at a given node, it propagates that information
//! along its outgoing edges. The most common edge type is `Flows`: for assignment statements like
//!
//! ```text
//! a = b
//! ```
//!
//! We might add to the graph
//!
//! ```text
//! b --Flows--> a
//! ```
//!
//! ## Edges are Directed
//!
//! If we learn that `b` is a map or scalar, then we learn the same about `a`. Unlike a
//! unification-based system, information does not flow bidirectionally: information about the type
//! of `b` flows into `a`, but not vice-versa. To see why, consider the following example,
//! (supposing all of these variables are global, and no SSA tricks can save us):
//!
//! ```text
//! b = 1
//! c = b + 1
//! a = b
//! a = a "3"
//! ```
//!
//! `b` is assigned a single integer value: we'd like to give it the type `Int`. However, `a` is
//! assigned to `b` as well as to the result of a string concatenation operation. String
//! concatenation produces a string, so it would seem that we have no choice but to give it the
//! type `Str`. That's what our algorithm does. It gives `b` and `c` type `Int` and `a` type `Str`,
//! necessitating an integer to string conversion in the third assignment expression. This is the
//! same number of coercions we would need with a fully dynamic representation of all the
//! variables. If, however, we let the type information for `a` flow back into `b`, then we would
//! find that `b` would have to be a string, and `c` would have to be a floating point number!
//!
//! ## Other Kinds of Edges
//!
//! In addition to `Flows`, some edges destructure map types. The `Key` constraint behaves like
//! `Flows`, but only works for `Key`; symmetically there are also `Val` constraints on maps. When
//! assigning to map keys, there is are also `KeyIn` and `MapIn`.
//!
//! For the statement:
//!
//! ```text
//! m[a] = 1
//! ```
//!
//! We would have the subgraph:
//!
//! ```text
//! +---ValIn---Int
//! |
//! v
//! m <--KeyIn-- a
//! ```
//!
//! Similarly, loading from a map creates a different constraint. For:
//!
//! ```text
//! x = m[a]
//! ```
//!
//! We would have
//!
//! ```text
//! x <--Val-- m <--KeyIn-- a
//! ```
//!
//! There are corresponding constraints for iterators (the objects emitted by foreach loops).
//!
//! ## Functions
//!
//! We mentioned earlier that functions made some of this trickier. Let's focus on addition:
//!
//! ```text
//! a = b + c
//! ```
//!
//! By this point in the pipeline, the `+` expression is regarded as calling a "builtin function".
//! Every builtin function provides a "best guess" on its result type given partial information.
//! The full implementation can be found in the [crate::builtins] module, but for addition it will
//! guess "integer" if it knows nothing or only knows about integer operands; if it sees a float or
//! string operand it will guess "float", and if it sees a non-scalar operand it will yield a type
//! error. Because functions can be arbitrary arity, this is materialized as a "hyper-edge" in the
//! graph from all operand nodes (`b` and `c` in this case) to the result node (`a`). Whenever any
//! operand updates, we will recompute our guess about the type of the result. There are a few more
//! subtleties here (functions can have out-params), but that's the basic idea.
//!
//! User-defined functions are a great deal more complicated. Some AWK functions are polymorphic;
//! consider the function:
//!
//! ```text
//! function x(a, b) { return length(a) + b; }
//! ```
//!
//! It is value to pass a string _or_ any map type to parameter `a`, and it is possible to pass any
//! scalar as parameter `b`. Depending on how you count, that could be a few dozen possible
//! function types! We monomorphize functions like this: we compute all the types `x` is called
//! with and generate a separate function for each of them. When we see a function call: we first
//! take our best guess of its argument types and check to see if we have a function corresponding
//! to those arguments. If we do not, then we allocate a new node for the return type of the
//! function and build a subgraph corresponding to it. This is a bit wasteful, as it leads to
//! duplicate functions for a single callsite; we can probably improve something on that front.
//!
//! # Solving Constraints
//!
//! Once we have this graph, can push values of type [`State`] around according to the constraints
//! along the edges. Once we reach a fixed point (i.e. our guess about the types of variables stops
//! changing), we return our answer. When the answer is under-specified, we just make a guess. For
//! statements like
//!
//! ```text
//! x = 1
//! ```
//!
//! We have the graph
//!
//! ```text
//! x <--Flows-- 1
//! ```
//!
//! It starts off as
//!
//! ```text
//! x <--Flows-- 1
//! ?            Scalar(Int)
//! ```
//!
//! And then stabilizes at
//!
//! ```text
//! x <--Flows-- 1
//! Scalar(Int)  Scalar(Int)
//! ```
//!
//! For the more complicated
//!
//! ```text
//! b = 1
//! c = b + 1
//! a = b
//! a = a "3"
//! ```
//!
//! We might have
//!
//! ```text
//! +--------
//! |       |
//! |       v
//! a <--(concat)<--3
//! ^
//! |
//! Flows
//! |
//! b <--Flows-- 1
//! |            |
//! v            |
//! +<-----------+
//! |
//! Call
//! |
//! v
//! c
//! ```
//!
//! We would start with `a`, `b` and `c` all having no information associated with them. The "no
//! information" output for `concat` is String, and for `+` is Int, so in the first full iteration
//! of the solver we would have a guess that `a` is a string, and that `b` and `c` are integers.
//! Doing another full iteration (now evaluating `+` with two integer values and `concat` with a
//! string and an integer) does not change these initial guesses; so the solver would return its
//! current solution.
//!
//! This sort of architecture is inspired by classic [static analysis algorithms] that find the least
//! fixed points of recursive equations phrased as monotone functions on some partial order with
//! finite height, along with [Propagators]
//!
//! [Propagators]: https://dspace.mit.edu/handle/1721.1/44215
//! [static analysis algorithms]: https://cs.au.dk/~amoeller/spa/
//! [Hindley-Milner]: https://en.wikipedia.org/wiki/Hindley%E2%80%93Milner_type_system
//! [`State`]: [crate::types::State]
use crate::ast;
use crate::builtins;
use crate::cfg::{self, Function, Ident, ProgramContext, is_unused};
use crate::common::{self, FileSpec, NodeIx, NumTy, Result};
use crate::compile;
use hashbrown::{HashMap, HashSet};

use std::ops::{Deref, DerefMut};

pub(crate) type SmallVec<T> = smallvec::SmallVec<[T; 2]>;

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub(crate) enum BaseTy {
    Null,
    Int,
    Float,
    Str,
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash)]
pub(crate) enum TVar<T> {
    Iter(T),
    Scalar(T),
    Map { key: T, val: T },
}

impl<T> TVar<T> {
    // deriving Functor ...
    fn map<S>(&self, mut f: impl FnMut(&T) -> S) -> TVar<S> {
        use TVar::*;
        match self {
            Iter(t) => Iter(f(t)),
            Scalar(t) => Scalar(f(t)),
            Map { key, val } => Map {
                key: f(key),
                val: f(val),
            },
        }
    }
}

impl<T: Clone> TVar<T> {
    pub(crate) fn abs(&self) -> Option<TVar<Option<T>>> {
        Some(self.map(|t| Some(t.clone())))
    }
}

pub(crate) fn val_of(s: &State) -> Result<State> {
    match s {
        Some(TVar::Map { val, .. }) => Ok(Some(TVar::Scalar(*val))),
        None => Ok(None),
        Some(TVar::Iter(_)) => err!("attempting to get value out of iterator state"),
        Some(TVar::Scalar(_)) => err!("attempting to get value out of iterator scalar"),
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub(crate) enum Constraint<T> {
    // TODO(ezr): Better names for Key/KeyIn etc.
    KeyIn(T),
    Key(T),
    ValIn(T),
    Val(T),
    IterVal(T),
    IterValIn(T),
    Flows(T),
    // Flows into the given node only if that node is an array (the array fallback of `try`).
    FlowsIfMap(T, NodeIx),
    // TODO(ezr): have a shared Vec and just store a slice here?
    CallBuiltin(SmallVec<NodeIx>, builtins::Function),
    CallUDF(
        NodeIx,           /* udf node */
        SmallVec<NodeIx>, /* arg nodes */
        NumTy,            /* cfg-level function ID */
    ),
}

impl<T> Constraint<T> {
    fn sub<S>(&self, s: S) -> Constraint<S> {
        match self {
            Constraint::KeyIn(_) => Constraint::KeyIn(s),
            Constraint::Key(_) => Constraint::Key(s),
            Constraint::ValIn(_) => Constraint::ValIn(s),
            Constraint::Val(_) => Constraint::Val(s),
            Constraint::IterValIn(_) => Constraint::IterValIn(s),
            Constraint::IterVal(_) => Constraint::IterVal(s),
            Constraint::Flows(_) => Constraint::Flows(s),
            Constraint::FlowsIfMap(_, to) => Constraint::FlowsIfMap(s, *to),
            Constraint::CallBuiltin(args, f) => Constraint::CallBuiltin(args.clone(), *f),
            Constraint::CallUDF(nix, args, f) => Constraint::CallUDF(*nix, args.clone(), *f),
        }
    }
    fn is_flow(&self) -> bool {
        matches!(self, Constraint::Flows(_))
    }
}

impl Constraint<State> {
    fn eval(&self, tc: &mut TypeContext) -> Result<State> {
        match self {
            Constraint::KeyIn(None) => Ok(Some(TVar::Map {
                key: None,
                val: None,
            })),
            // Numeric keys are converted to strings in awk; maps only have Int or Str keys, so
            // floating-point keys make the map Str-keyed.
            Constraint::KeyIn(Some(TVar::Scalar(k))) => Ok(Some(TVar::Map {
                key: k.map(|k| if k == BaseTy::Float { BaseTy::Str } else { k }),
                val: None,
            })),
            Constraint::KeyIn(op) => err!("Non-scalar KeyIn constraint: {:?}", op),

            Constraint::Key(None) => Ok(None),
            Constraint::Key(Some(TVar::Map { key: s, .. })) => Ok(Some(TVar::Scalar(*s))),
            Constraint::Key(op) => {
                err!("invalid operand for Key constraint: {:?} (must be map)", op)
            }

            Constraint::ValIn(None) => Ok(Some(TVar::Map {
                key: None,
                val: None,
            })),
            Constraint::ValIn(Some(TVar::Scalar(v))) => Ok(Some(TVar::Map { key: None, val: *v })),
            Constraint::ValIn(op) => err!("Non-scalar ValIn constraint: {:?}", op),

            Constraint::Val(None) => Ok(None),
            Constraint::Val(Some(TVar::Map { val: s, .. })) => Ok(Some(TVar::Scalar(*s))),
            Constraint::Val(op) => {
                err!("invalid operand for Val constraint: {:?} (must be map)", op)
            }

            Constraint::IterValIn(None) => Ok(Some(TVar::Iter(None))),
            Constraint::IterValIn(Some(TVar::Scalar(v))) => Ok(Some(TVar::Iter(*v))),
            Constraint::IterValIn(op) => err!("Non-scalar IterValIn constraint: {:?}", op),

            Constraint::IterVal(None) => Ok(None),
            Constraint::IterVal(Some(TVar::Iter(s))) => Ok(Some(TVar::Scalar(*s))),
            Constraint::IterVal(op) => err!(
                "invalid operand for IterVal constraint: {:?} (must be iterator)",
                op
            ),

            Constraint::Flows(s) => Ok(*s),
            Constraint::FlowsIfMap(s, to) => match tc.nw.read(*to) {
                Some(TVar::Map { .. }) => Ok(*s),
                _ => Ok(None),
            },
            Constraint::CallBuiltin(args, f) => {
                let arg_state: SmallVec<State> = args.iter().map(|ix| *tc.nw.read(*ix)).collect();
                f.step(&arg_state[..])
            }
            Constraint::CallUDF(nix, args, f) => {
                if !tc.allow_null_args && args.iter().any(|ix| tc.nw.read(*ix).is_none()) {
                    return Ok(None);
                }
                let ret_ix = tc.get_function(&tc.func_table[*f as usize], args.clone(), *nix);
                Ok(*tc.nw.read(ret_ix))
            }
        }
    }
}

// We distinguish Edge from Constraint because we want to add more data later.
#[derive(Clone)]
struct Edge {
    // TODO: do we need a separate edge type, or can it just be Constraint<()>
    constraint: Constraint<()>,
}

// encode deps as a single edge from function to Var?

#[derive(Copy, Clone)]
enum Rule {
    Var,
    Const(State),
    AlwaysNotify,
}

pub(crate) type State = Option<TVar<Option<BaseTy>>>;

pub(crate) fn null_ty() -> compile::Ty {
    flatten(concrete(None)).unwrap()
}

impl Rule {
    // TODO(ezr): Why also have `prev`? This allows us to only hand the deps back that have changed
    // since the last update. this extra functionality is not yet implemented; it's unclear if we
    // can use it while still relying on petgraph, or if we'll have to (e.g.) store a priority
    // queue of edges per node ordered by modified timestamp.
    fn step(&self, prev: &State, deps: &[State]) -> Result<(bool, State)> {
        fn value_rule(b1: BaseTy, b2: BaseTy) -> BaseTy {
            use BaseTy::*;
            match (b1, b2) {
                (Null, x) | (x, Null) => x,
                (Str, _) | (_, Str) => Str,
                (Float, _) | (_, Float) => Float,
                (Int, Int) => Int,
            }
        }
        if let Rule::Const(tv) = self {
            return Ok((tv != prev, *tv));
        }
        if let Rule::AlwaysNotify = self {
            return Ok((true, None));
        }
        let mut cur = *prev;
        for d in deps.iter().cloned() {
            use TVar::*;
            cur = match (cur, d) {
                (None, x) | (x, None) => x,
                (Some(x), Some(y)) => match (x, y) {
                    (Iter(x), Iter(None)) | (Iter(None), Iter(x)) => Some(Iter(x)),
                    // Iterators range over map keys, so they widen like map keys do (see
                    // join_key below): a key seen as Int and later as Str/Float becomes Str.
                    (Iter(Some(x)), Iter(Some(y))) => match (x, y) {
                        _ if x == y => cur,
                        (BaseTy::Null, z) | (z, BaseTy::Null) => Some(Iter(Some(z))),
                        _ => Some(Iter(Some(BaseTy::Str))),
                    },
                    (Scalar(x), Scalar(None)) | (Scalar(None), Scalar(x)) => Some(Scalar(x)),
                    (Scalar(Some(x)), Scalar(Some(y))) => Some(Scalar(Some(value_rule(x, y)))),
                    (Map { key: k1, val: v1 }, Map { key: k2, val: v2 }) => {
                        fn join_key(b1: BaseTy, b2: BaseTy) -> BaseTy {
                            use BaseTy::*;
                            match (b1, b2) {
                                (Float, _)
                                | (_, Float)
                                | (Str, _)
                                | (_, Str)
                                | (Null, _)
                                | (_, Null) => Str,
                                (Int, _) => Int,
                            }
                        }
                        fn lift(
                            f: impl Fn(BaseTy, BaseTy) -> BaseTy,
                            o1: Option<BaseTy>,
                            o2: Option<BaseTy>,
                        ) -> Option<BaseTy> {
                            match (o1, o2) {
                                (Some(x), Some(y)) => Some(f(x, y)),
                                (Some(x), None) | (None, Some(x)) => Some(x),
                                (None, None) => None,
                            }
                        }
                        Some(Map {
                            key: lift(join_key, k1, k2),
                            val: lift(value_rule, v1, v2),
                        })
                    }
                    (Map { .. }, Scalar(_)) | (Scalar(_), Map { .. }) => {
                        return err!(
                            "a variable is used both as an array and as a scalar \
                             (arrays of arrays such as `a[i][j]` are not supported)"
                        );
                    }
                    (t1, t2) => return err!("kinds do not match. {:?} vs {:?}", t1, t2),
                },
            };
        }
        Ok((prev != &cur, cur))
    }
}

/// The name of the array type `ty` as written in the function docs, e.g. `map[int]str`.
fn array_ty_name(ty: compile::Ty) -> String {
    let base = |t: Result<compile::Ty>| match t {
        Ok(compile::Ty::Int) => "int",
        Ok(compile::Ty::Float) => "float",
        _ => "str",
    };
    format!("map[{}]{}", base(ty.key()), base(ty.val()))
}

fn concrete(state: State) -> TVar<BaseTy> {
    fn concrete_scalar(o: &Option<BaseTy>) -> BaseTy {
        o.unwrap_or(BaseTy::Null)
    }
    match state {
        Some(x) => x.map(concrete_scalar),
        None => TVar::Scalar(BaseTy::Null),
    }
}

fn flatten(tv: TVar<BaseTy>) -> Result<compile::Ty> {
    use compile::Ty;
    use {BaseTy::*, TVar::*};
    fn flatten_base(b: BaseTy) -> Ty {
        match b {
            Int => Ty::Int,
            Float => Ty::Float,
            Str => Ty::Str,
            Null => Ty::Null,
        }
    }
    match tv {
        Scalar(b) => Ok(flatten_base(b)),
        Iter(Int) => Ok(Ty::IterInt),
        Iter(Null) | Iter(Str) => Ok(Ty::IterStr),
        Iter(x) => err!("Iterator over an unsupported type: {:?}", x),
        Map { key, val } => {
            let f = |ty| {
                if ty == Null {
                    Ty::Str
                } else {
                    flatten_base(ty)
                }
            };
            match (f(key), f(val)) {
                (Ty::Int, Ty::Int) => Ok(Ty::MapIntInt),
                (Ty::Int, Ty::Float) => Ok(Ty::MapIntFloat),
                (Ty::Int, Ty::Str) => Ok(Ty::MapIntStr),
                (Ty::Str, Ty::Int) => Ok(Ty::MapStrInt),
                (Ty::Str, Ty::Float) => Ok(Ty::MapStrFloat),
                (Ty::Str, Ty::Str) => Ok(Ty::MapStrStr),
                (k, v) => err!("Map with unsupported type (key={:?} val={:?})", k, v),
            }
        }
    }
}

#[derive(Clone)]
struct Node {
    rule: Rule,
    cur_val: State,
    // TODO(ezr): put debugging information in here?
}

impl Node {
    fn new(rule: Rule) -> Node {
        Node {
            rule,
            cur_val: None,
        }
    }
}

pub(crate) struct Network {
    base_node: NodeIx,
    wl: common::WorkList<NodeIx>,
    call_deps: HashMap<NodeIx, SmallVec<NodeIx>>,
    graph: common::Graph<Node, Edge>,
    iso: HashSet<(NumTy, NumTy)>,
}

impl Default for Network {
    fn default() -> Network {
        let mut graph = common::Graph::default();
        let base_node = graph.add_node(Node::new(Rule::Var));
        Network {
            graph,
            base_node,
            wl: Default::default(),
            call_deps: Default::default(),
            iso: Default::default(),
        }
    }
}

fn is_iso(set: &HashSet<(NumTy, NumTy)>, n1: NodeIx, n2: NodeIx) -> bool {
    use std::cmp::{max, min};
    let i1 = n1.index() as NumTy;
    let i2 = n2.index() as NumTy;
    set.contains(&(min(i1, i2), max(i1, i2)))
}

fn make_iso(set: &mut HashSet<(NumTy, NumTy)>, n1: NodeIx, n2: NodeIx) -> bool {
    use std::cmp::{max, min};
    let i1 = n1.index() as NumTy;
    let i2 = n2.index() as NumTy;
    set.insert((min(i1, i2), max(i1, i2)))
}

impl Network {
    fn add_rule(&mut self, rule: Rule) -> NodeIx {
        let res = self.graph.add_node(Node::new(rule));
        self.wl.insert(res);
        res
    }

    fn read(&self, ix: NodeIx) -> &State {
        &self.graph.node_weight(ix).unwrap().cur_val
    }

    pub(crate) fn add_dep(&mut self, from: NodeIx, to: NodeIx, constraint: Constraint<()>) {
        self.graph.add_edge(from, to, Edge { constraint });
        self.wl.insert(from);
    }
}

pub(crate) struct TypeContext<'a, 'b> {
    pub(crate) nw: Network,
    base: HashMap<State, NodeIx>,
    env: HashMap<Args<Ident>, NodeIx>,
    funcs: HashMap<Args<NumTy>, NodeIx>,
    maps: HashSet<NodeIx>,
    func_table: &'a [Function<'b, &'b str>],
    local_globals: &'a HashSet<NumTy>,
    udf_nodes: Vec<NodeIx>,
    // Names of global variables, for error messages.
    global_names: HashMap<NumTy, &'b str>,
    use_locs: &'a crate::cfg::UseLocs,
    // Whether a user-defined function may be instantiated for a call with untyped arguments.
    // Such an instance is shared by every call whose arguments are untyped at the time, which
    // unifies their types (e.g. arrays with int and str keys), so these calls wait for their
    // arguments to be typed first; calls whose arguments stay untyped are handled afterwards.
    allow_null_args: bool,
}

struct View<'a, 'b, 'c> {
    tc: &'a mut TypeContext<'b, 'c>,
    frame_id: NumTy, // which function are we in?
    frame_args: SmallVec<State>,
}

impl<'a, 'b, 'c> Deref for View<'a, 'b, 'c> {
    type Target = &'a mut TypeContext<'b, 'c>;
    fn deref(&self) -> &&'a mut TypeContext<'b, 'c> {
        &self.tc
    }
}

impl<'a, 'b, 'c> DerefMut for View<'a, 'b, 'c> {
    fn deref_mut(&mut self) -> &mut &'a mut TypeContext<'b, 'c> {
        &mut self.tc
    }
}

pub(crate) fn get_types<'a>(pc: &ProgramContext<'a, &'a str>) -> Result<TypeInfo> {
    TypeContext::from_prog(pc)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Args<T> {
    id: T,
    func_id: Option<NumTy>,
    args: SmallVec<State>, // ignored when id.global
}

pub(crate) struct TypeInfo {
    // Map a particular identifier in a function to a type.
    pub var_tys: HashMap<(Ident, NumTy, SmallVec<compile::Ty>), compile::Ty>,
    // Map a particular function invocation to a return type.
    pub func_tys: HashMap<(NumTy, SmallVec<compile::Ty>), compile::Ty>,
}

impl<'b, 'c> TypeContext<'b, 'c> {
    fn from_pc(pc: &'b ProgramContext<'c, &'c str>) -> TypeContext<'b, 'c> {
        let mut tc = TypeContext {
            nw: Default::default(),
            base: Default::default(),
            env: Default::default(),
            funcs: Default::default(),
            maps: Default::default(),
            func_table: &pc.funcs[..],
            local_globals: pc.local_globals_ref(),
            udf_nodes: Default::default(),
            global_names: pc.global_names(),
            use_locs: pc.use_locs(),
            allow_null_args: false,
        };
        tc.udf_nodes = (0..pc.funcs.len())
            .map(|_| tc.nw.add_rule(Rule::AlwaysNotify))
            .collect();
        tc
    }
    pub(crate) fn from_prog<'a>(pc: &ProgramContext<'a, &'a str>) -> Result<TypeInfo> {
        use hashbrown::hash_map::Entry;
        let mut tc = TypeContext::from_pc(pc);
        // TODO: to migrate, simply iterate over the Stage variant of this and solve at the end?
        for offset in pc.main_offsets() {
            let main = &pc.funcs[offset];
            let main_base = tc.udf_nodes[offset];
            tc.get_function(main, /*arg_nodes=*/ Default::default(), main_base);
        }

        let mut res = tc.solve();
        if res.is_ok() {
            tc.allow_null_args = true;
            let calls: Vec<NodeIx> = tc
                .nw
                .graph
                .raw_edges()
                .iter()
                .filter(|e| matches!(e.weight.constraint, Constraint::CallUDF(..)))
                .map(|e| e.target())
                .collect();
            for c in calls {
                tc.nw.wl.insert(c);
            }
            res = tc.solve();
        }
        if let Err(e) = res {
            // A scalar local used as an array may make another value conflict first, as may a
            // scalar passed to an array parameter of a builtin.
            tc.check_scalar_locals()?;
            tc.check_builtin_array_params()?;
            return Err(e);
        }
        tc.check_builtin_array_params()?;
        tc.check_builtin_args()?;
        tc.check_udf_array_args()?;
        tc.check_scalar_locals()?;
        tc.check_ssa_kinds()?;
        let mut var_tys = HashMap::new();
        let mut func_tys = HashMap::new();
        for (Args { id, args, .. }, ix) in tc.funcs.iter() {
            let mut flat_args = SmallVec::new();
            for a in args.iter().cloned() {
                flat_args.push(flatten(concrete(a))?);
            }
            if let Entry::Vacant(v) = func_tys.entry((*id, flat_args)) {
                v.insert(flatten(concrete(*tc.nw.read(*ix)))?);
            }
        }
        for (Args { id, func_id, args }, ix) in tc.env.iter() {
            // The placeholder variable has no register (see `ident_node`).
            if is_unused(*id) {
                continue;
            }
            let mut flat_args = SmallVec::new();
            for a in args.iter().cloned() {
                flat_args.push(flatten(concrete(a))?);
            }
            let v = flatten(concrete(*tc.nw.read(*ix)))?;

            // We won't use the function id if id.global, so setting it to 0 should be fine.
            // TODO clean up some of this to make it less misleading
            match var_tys.entry((*id, func_id.unwrap_or(0), flat_args)) {
                Entry::Vacant(vac) => {
                    vac.insert(v);
                }
                Entry::Occupied(mut occ) => {
                    // In an earlier iteration wrt args, a variable could have been assigned Int,
                    // but is now assigned Float. Pick the float copy, otherwise signal an error.
                    //
                    // TODO: this is a bit of a hack, This is only necessary because we aren't
                    // filtering out calls to functions with (None) arguments, after solving. In
                    // many cases, those functions are not here anymore. We should key this on
                    // unflattened args.
                    let prev = *occ.get();
                    if prev != v {
                        use compile::Ty;
                        match (prev, v) {
                            // TODO: unclear if this is safe in all cases of Str => Float. It only
                            // makes sense when the Str _was_ null. Perhaps we can propagate that
                            // null information into this map.
                            (Ty::Float, _) => {}
                            (_, Ty::Float) => {
                                occ.insert(v);
                            }
                            _ => {
                                return err!(
                                    "coherence violation! (func_id={:?}) {:?} in args {:?}, we get both {:?} and {:?}\nenv={:?}",
                                    func_id,
                                    id,
                                    args,
                                    v,
                                    prev,
                                    tc.env
                                );
                            }
                        }
                    }
                }
            }
        }
        Ok(TypeInfo { var_tys, func_tys })
    }
    fn solve(&mut self) -> Result<()> {
        let mut dep_indices: SmallVec<NodeIx> = Default::default();
        let mut deps: SmallVec<State> = Default::default();
        // For each entry of `deps`: whether it comes from using the node as an array (`a[k]`,
        // `k in a`, ...) rather than from an array value flowing into it.
        let mut array_uses: SmallVec<bool> = Default::default();
        // TODO are updates to UDFs getting "lost" somehow? It seems like it isn't just changes in
        // arguments that could cause a change in the return type (imagine returning a global
        // variable from within a function). How do we ensure we see those updates?
        //  - Each call edge (or each UDF?) has its own node. Changes to return type cause that node to be
        //  inserted into the worklist? Downside is that all callsites are woken up; but we may not
        //  have a choice?
        while let Some(ix) = self.nw.wl.pop() {
            deps.clear();
            dep_indices.clear();
            array_uses.clear();
            let Node { rule, cur_val } = self.nw.graph.node_weight(ix).unwrap().clone();
            // Iterate over the incoming edges; read their current values and evaluate the
            // constraints.
            use petgraph::Direction::Incoming;
            let mut walker = self.nw.graph.neighbors_directed(ix, Incoming).detach();
            while let Some((e_ix, node_ix)) = walker.next(&self.nw.graph) {
                let edge = self.nw.graph.edge_weight(e_ix).unwrap().clone();
                let node_val = self.nw.graph.node_weight(node_ix).unwrap().cur_val;
                match edge.constraint.sub(node_val).eval(self) {
                    Ok(dep) => {
                        deps.push(dep);
                        let from_const = matches!(
                            self.nw.graph.node_weight(node_ix).unwrap().rule,
                            Rule::Const(_)
                        );
                        array_uses.push(match edge.constraint {
                            Constraint::KeyIn(_) | Constraint::ValIn(_) => true,
                            Constraint::Flows(_) => from_const,
                            _ => false,
                        });
                    }
                    Err(e) => {
                        // Key/Val constraints read a map; the others build one from a scalar.
                        let as_array = matches!(
                            edge.constraint,
                            Constraint::Key(_) | Constraint::Val(_) | Constraint::IterVal(_)
                        );
                        let scalar_as_array = match (as_array, node_val) {
                            (true, Some(TVar::Scalar(_))) => true,
                            (false, Some(TVar::Map { .. })) => false,
                            _ => return Err(e),
                        };
                        return Err(self.kind_error(node_ix, scalar_as_array));
                    }
                }
                if edge.constraint.is_flow() {
                    dep_indices.push(node_ix);
                }
            }
            // Compute an update value based on the newly-evaluated constraints.
            let (changed, next) = match rule.step(&cur_val, &deps[..]) {
                Ok(res) => res,
                // A node that is (so far) a map receiving a scalar, or vice versa.
                Err(e) => {
                    let is_map = |d: &State| matches!(d, Some(TVar::Map { .. }));
                    let has_scalar = matches!(cur_val, Some(TVar::Scalar(_)))
                        || deps.iter().any(|d| matches!(d, Some(TVar::Scalar(_))));
                    let maps = deps
                        .iter()
                        .zip(array_uses.iter())
                        .filter(|(d, _)| is_map(d));
                    let (mut array_use, mut array_value) = (false, is_map(&cur_val));
                    for (_, used) in maps {
                        if *used {
                            array_use = true;
                        } else {
                            array_value = true;
                        }
                    }
                    if !has_scalar {
                        return Err(e);
                    }
                    if array_value {
                        return Err(self.kind_error(ix, false));
                    }
                    if array_use {
                        return Err(self.kind_error(ix, true));
                    }
                    return Err(e);
                }
            };
            if !changed {
                continue;
            }
            if let Some(TVar::Map { .. }) = next {
                // If we have a map node, then we need to make sure that anything that assigns to
                // it winds up with the same type. Assignments are marked as `Flows` constraints;
                // and we recorded the nodes that flow into `ix` in `dep_indices`. Now we just add
                // another arrow back, unless we have done so before. We figure that last part out
                // by using the `iso` functions, which take care to sort the indices before before
                // inserting them into the hash set so we only store them once.
                for d in dep_indices.iter().cloned() {
                    if is_iso(&self.nw.iso, ix, d) {
                        continue;
                    }
                    self.nw.graph.add_edge(
                        ix,
                        d,
                        Edge {
                            constraint: Constraint::Flows(()),
                        },
                    );
                    make_iso(&mut self.nw.iso, ix, d);
                }
            }
            self.nw.graph.node_weight_mut(ix).unwrap().cur_val = next;
            for n in self.nw.graph.neighbors(ix) {
                self.nw.wl.insert(n);
            }
            for c in self.nw.call_deps.get(&ix).iter().flat_map(|c| c.iter()) {
                self.nw.wl.insert(*c);
            }
        }
        Ok(())
    }

    /// Check that no scalar is passed to a parameter of a builtin function that must be an array,
    /// e.g. `split(s, 5)`. Done before reporting other errors: the array flowing into the scalar
    /// argument makes the conflict show up elsewhere, or only when the call is compiled.
    fn check_builtin_array_params(&self) -> Result<()> {
        use petgraph::visit::EdgeRef;
        for edge in self.nw.graph.raw_edges() {
            let Constraint::CallBuiltin(args, f) = &edge.weight.constraint else {
                continue;
            };
            let Some(name) = f.awk_name() else { continue };
            for &i in f.array_params() {
                let Some(arg) = args.get(i) else { continue };
                // A variable may already have become an array when the scalar assigned to it
                // conflicted: check the values flowing into it too.
                let is_scalar = |ix: NodeIx| matches!(self.nw.read(ix), Some(TVar::Scalar(Some(b))) if *b != BaseTy::Null);
                let receives_scalar = self
                    .nw
                    .graph
                    .edges_directed(*arg, petgraph::Direction::Incoming)
                    .any(|e| {
                        matches!(e.weight().constraint, Constraint::Flows(_))
                            && is_scalar(e.source())
                    });
                if !is_scalar(*arg) && !receives_scalar {
                    continue;
                }
                let what = match self.var_name(*arg) {
                    Some(d) => format!("{} is a scalar", d),
                    None => "a scalar value is given".to_string(),
                };
                let msg = format!(
                    "{}: argument {} must be an array, but {}",
                    name,
                    i + 1,
                    what
                );
                let err = common::CompileError::new(msg);
                return Err(match self.use_locs.builtin_calls.get(name) {
                    Some(loc) => err.with_loc(*loc),
                    None => err,
                });
            }
        }
        Ok(())
    }

    /// Check that the arguments of builtin functions are arrays exactly where the functions expect
    /// arrays, e.g. that an array is not passed to a function parameter used as a scalar.
    fn check_builtin_args(&self) -> Result<()> {
        for edge in self.nw.graph.raw_edges() {
            let Constraint::CallBuiltin(args, f) = &edge.weight.constraint else {
                continue;
            };
            let mut tys = SmallVec::new();
            for a in args.iter() {
                tys.push(flatten(concrete(*self.nw.read(*a)))?);
            }
            // Other errors in the signature are reported when the call is compiled.
            let Ok((expected, ret)) = f.type_sig(&tys[..]) else {
                continue;
            };
            // An array result is not converted either: it must keep the type of the builtin.
            let res_ty = flatten(concrete(*self.nw.read(edge.target())))?;
            if ret.is_array() && res_ty.is_array() && ret != res_ty {
                let name = f.awk_name().unwrap_or("builtin");
                let msg = format!(
                    "the array returned by `{}()` has type {}, but it is used as an array of type {} \
                     (e.g. passed to a function parameter that also receives arrays of another type)",
                    name,
                    array_ty_name(ret),
                    array_ty_name(res_ty),
                );
                let err = common::CompileError::new(msg);
                return Err(match self.use_locs.builtin_calls.get(name) {
                    Some(loc) => err.with_loc(*loc),
                    None => err,
                });
            }
            for (i, ((arg, ty), exp)) in
                args.iter().zip(tys.iter()).zip(expected.iter()).enumerate()
            {
                if ty.is_array() != exp.is_array() && *ty != compile::Ty::Null {
                    let err = self.kind_error(*arg, !ty.is_array());
                    // E.g. a literal: report the call instead.
                    let loc = f
                        .awk_name()
                        .and_then(|name| self.use_locs.builtin_calls.get(name));
                    return Err(match (err.1, loc) {
                        (None, Some(loc)) => err.with_loc(*loc),
                        _ => err,
                    });
                }
                // Arrays are not converted between key and value types.
                if ty.is_array() && ty != exp {
                    return Err(self.array_arg_error(f, i, *arg, *ty, *exp));
                }
            }
        }
        Ok(())
    }

    /// Check that an array passed to a user-defined function has the type of the parameter it is
    /// passed to: arrays are passed by reference, so they are never converted. This fails when a
    /// parameter receives arrays of different types (e.g. with int and str keys) from different
    /// calls whose types could not be told apart.
    fn check_udf_array_args(&self) -> Result<()> {
        for edge in self.nw.graph.raw_edges() {
            let Constraint::CallUDF(_, arg_nodes, f) = &edge.weight.constraint else {
                continue;
            };
            // The instance of the function for the final types of the arguments, as in
            // `get_function` (missing arguments are null).
            let func = &self.func_table[*f as usize];
            let mut args: SmallVec<State> = arg_nodes.iter().map(|ix| *self.nw.read(*ix)).collect();
            args.resize(func.args.len(), None);
            if !self.funcs.contains_key(&Args {
                id: *f,
                func_id: None,
                args: args.clone(),
            }) {
                continue;
            }
            for (param, arg) in func.args.iter().zip(args.iter()) {
                let key = Args {
                    id: param.id,
                    func_id: Some(*f),
                    args: args.clone(),
                };
                let Some(ix) = self.env.get(&key) else {
                    continue;
                };
                let arg_ty = flatten(concrete(*arg))?;
                let param_ty = flatten(concrete(*self.nw.read(*ix)))?;
                if !arg_ty.is_array() || !param_ty.is_array() || arg_ty == param_ty {
                    continue;
                }
                let msg = format!(
                    "parameter `{}` of function `{}` receives arrays of different types ({} and {}); \
                     zawk requires the arrays passed to a parameter to have the same key and value types, \
                     e.g. use separate functions, or copy the array into one with string keys",
                    ast::source_name(&param.name.to_string()),
                    func.name,
                    array_ty_name(arg_ty),
                    array_ty_name(param_ty),
                );
                let err = common::CompileError::new(msg);
                return Err(match self.use_locs.udf_calls.get(f) {
                    Some(loc) => err.with_loc(*loc),
                    None => err,
                });
            }
        }
        Ok(())
    }

    /// An error for the array `arg`, of type `ty`, passed as argument `i` of the builtin `f`,
    /// which expects an array of type `exp`.
    fn array_arg_error(
        &self,
        f: &builtins::Function,
        i: usize,
        arg: NodeIx,
        ty: compile::Ty,
        exp: compile::Ty,
    ) -> common::CompileError {
        let name = f.awk_name().unwrap_or("builtin");
        let hint = match exp.key() {
            Ok(compile::Ty::Int) if ty.key().ok() != Some(compile::Ty::Int) => {
                " with integer keys (a[1], a[2], ...)"
            }
            _ => "",
        };
        let what = match self.describe(arg) {
            Some(d) => format!("array {}", d),
            None => "the array".to_string(),
        };
        let msg = format!(
            "{}: argument {} must be an array{} of type {}, but {} has type {}",
            name,
            i + 1,
            hint,
            array_ty_name(exp),
            what,
            array_ty_name(ty)
        );
        let err = common::CompileError::new(msg);
        match self.use_locs.builtin_calls.get(name) {
            Some(loc) => err.with_loc(*loc),
            None => err,
        }
    }

    /// Check that no scalar local (`local x`) is used as an array. Its declaration assigns it its
    /// own `uninit_var`, which only becomes an array when the local is one.
    fn check_scalar_locals(&self) -> Result<()> {
        for (Args { id, func_id, .. }, ix) in self.env.iter() {
            // Globals referenced only from the main program are local to it.
            let name = func_id
                .and_then(|f| {
                    self.func_table[f as usize]
                        .args
                        .iter()
                        .find(|a| a.id.low == id.low)
                })
                .map(|a| a.name)
                .or_else(|| self.global_names.get(&id.low).copied());
            if !name.is_some_and(ast::is_uninit_var)
                || !matches!(self.nw.read(*ix), Some(TVar::Map { .. }))
            {
                continue;
            }
            for n in self.nw.graph.neighbors(*ix) {
                let Some(local) = self.var_name(n) else {
                    continue;
                };
                let err = self.kind_error(n, true);
                let hint = local.split('`').nth(1).unwrap_or("x");
                return Err(common::CompileError(
                    format!("{} (declare it with `local {}[]`)", err.0, hint),
                    err.1,
                ));
            }
        }
        Ok(())
    }

    /// Check that no variable is both an array and a scalar. Assigning to a scalar variable
    /// introduces a new SSA version of it with its own type node, so `x[1] = 1; x = 2` does not
    /// produce a conflict during solving: the two versions of `x` must be compared here.
    fn check_ssa_kinds(&self) -> Result<()> {
        let mut kinds: HashMap<(Ident, Option<NumTy>, &[State]), (Option<NodeIx>, bool)> =
            HashMap::new();
        for (Args { id, func_id, args }, ix) in self.env.iter() {
            // The placeholder variable receives discarded values of every kind.
            if is_unused(*id) {
                continue;
            }
            let entry = kinds
                .entry((id._base(), *func_id, &args[..]))
                .or_insert((None, false));
            match self.nw.read(*ix) {
                Some(TVar::Map { .. }) => entry.0 = Some(*ix),
                // Versions that are never assigned a value (e.g. the initial one) stay null.
                Some(TVar::Scalar(Some(b))) if *b != BaseTy::Null => entry.1 = true,
                _ => {}
            }
        }
        for (map_ix, has_scalar) in kinds.values() {
            if let (Some(ix), true) = (map_ix, has_scalar) {
                return Err(self.kind_error(*ix, false));
            }
        }
        Ok(())
    }

    /// An error for a value that is used both as an array and as a scalar: a scalar used as an
    /// array if `scalar_as_array`, otherwise an array used as a scalar. As in gawk, the message
    /// names the variable when there is one.
    fn kind_error(&self, ix: NodeIx, scalar_as_array: bool) -> common::CompileError {
        let desc = self.describe(ix);
        let msg = match (scalar_as_array, desc) {
            (true, Some(d)) => format!("attempt to use scalar {} as an array", d),
            (true, None) => "attempt to use a scalar value as an array".to_string(),
            (false, Some(d)) => format!("attempt to use array {} in a scalar context", d),
            (false, None) => "attempt to use an array in a scalar context".to_string(),
        };
        let err = common::CompileError::new(msg);
        match self.kind_error_loc(ix, scalar_as_array) {
            Some(loc) => err.with_loc(loc),
            None => err,
        }
    }

    /// Where the value at `ix` is used the wrong way: for a variable, its first use as an array
    /// if `scalar_as_array`, otherwise its first use as a scalar (or else its first use); for the
    /// result of a builtin, the first call of the builtin.
    fn kind_error_loc(&self, ix: NodeIx, scalar_as_array: bool) -> Option<crate::lexer::Loc> {
        use petgraph::Direction::Incoming;
        let uses = self.use_locs;
        let var_loc = |ix: NodeIx| {
            let (key, _) = self.env.iter().find(|(_, v)| **v == ix)?;
            let low = key.id.low;
            let kind = if scalar_as_array {
                &uses.array
            } else {
                &uses.scalar
            };
            kind.get(&low).or_else(|| uses.any.get(&low)).copied()
        };
        if let Some(loc) = var_loc(ix) {
            return Some(loc);
        }
        // As in `describe`: the value may flow in from a variable or a builtin call.
        for n in self.nw.graph.neighbors_directed(ix, Incoming) {
            if let Some(loc) = var_loc(n) {
                return Some(loc);
            }
            for e in self.nw.graph.edges_directed(n, Incoming) {
                if let Constraint::CallBuiltin(_, f) = &e.weight().constraint {
                    if let Some(loc) = f.awk_name().and_then(|name| uses.builtin_calls.get(name)) {
                        return Some(*loc);
                    }
                }
            }
        }
        None
    }

    /// Describe the value at `ix` for an error message: the variable it belongs to, or the
    /// builtin function that computed it.
    fn describe(&self, ix: NodeIx) -> Option<String> {
        use petgraph::Direction::Incoming;
        if let Some(d) = self.var_name(ix) {
            return Some(d);
        }
        // Values flow into a node through intermediate nodes, such as the one holding the result
        // of a builtin call.
        for n in self.nw.graph.neighbors_directed(ix, Incoming) {
            if let Some(d) = self.var_name(n) {
                return Some(d);
            }
            for e in self.nw.graph.edges_directed(n, Incoming) {
                if let Constraint::CallBuiltin(_, f) = &e.weight().constraint {
                    if let Some(name) = f.awk_name() {
                        return Some(format!("returned by `{}()`", name));
                    }
                }
            }
        }
        None
    }

    /// The name of the variable whose type is the node `ix`, if any.
    fn var_name(&self, ix: NodeIx) -> Option<String> {
        let (key, _) = self.env.iter().find(|(_, v)| **v == ix)?;
        let low = key.id.low;
        if let Some(func_id) = key.func_id {
            let func = &self.func_table[func_id as usize];
            if let Some(arg) = func.args.iter().find(|a| a.id.low == low) {
                let name = arg.name.to_string();
                let name = ast::source_name(&name);
                return Some(if ast::is_local_name(&arg.name.to_string()) {
                    format!("`{}` (local variable of function `{}`)", name, func.name)
                } else {
                    format!("`{}` (parameter of function `{}`)", name, func.name)
                });
            }
        }
        // Globals referenced only from the main program are local to it.
        self.global_names
            .get(&low)
            .map(|name| format!("`{}`", ast::source_name(name)))
    }

    pub(crate) fn constant(&mut self, tv: State) -> NodeIx {
        use hashbrown::hash_map::Entry::*;
        match self.base.entry(tv) {
            Occupied(o) => *o.get(),
            Vacant(v) => {
                let res = self.nw.add_rule(Rule::Const(tv));
                v.insert(res);
                res
            }
        }
    }
    /// A new type variable, e.g. to relate the types of builtin function arguments.
    pub(crate) fn fresh_var(&mut self) -> NodeIx {
        self.nw.add_rule(Rule::Var)
    }

    pub(crate) fn constrain_as_map(&mut self, ix: NodeIx) {
        // To be completely explicit, this function assigns a unique `Flows` constraint into a map
        // from the constant node that "just specifies the node is a Map".
        if self.maps.insert(ix) {
            let is_map = self.constant(Some(TVar::Map {
                key: None,
                val: None,
            }));
            self.nw.add_dep(is_map, ix, Constraint::Flows(()))
        }
    }
    pub(crate) fn get_node(&mut self, key: Args<Ident>) -> NodeIx {
        *self.env.entry(key).or_insert(self.nw.add_rule(Rule::Var))
    }

    fn get_function<'a>(
        &mut self,
        Function {
            ident, cfg, args, ..
        }: &Function<'a, &'a str>,
        mut arg_nodes: SmallVec<NodeIx>,
        base_node: NodeIx,
    ) -> NodeIx {
        // First we want to normalize the provided arguments. If we provide too few arguments, the
        // rest are filled with nulls. If we provide too many arguments, we throw away the extras.
        if arg_nodes.len() < args.len() {
            for _ in 0..(args.len() - arg_nodes.len()) {
                arg_nodes.push(self.constant(None));
            }
        }
        if args.len() < arg_nodes.len() {
            arg_nodes.truncate(args.len());
        }

        let arg_states = arg_nodes.iter().map(|ix| *self.nw.read(*ix)).collect();

        let key = Args {
            id: *ident,
            func_id: None,
            args: arg_states,
        };

        // Check if we have already created the function
        if let Some(ix) = self.funcs.get(&key).cloned() {
            // The instance is shared with earlier call sites, but this call's arguments still
            // need to flow into the parameters; otherwise constraints inferred for the parameters
            // (e.g. "is a map") never reach these arguments.
            let mut view = View {
                tc: self,
                frame_id: *ident,
                frame_args: key.args,
            };
            for (cfg::Arg { id, .. }, arg_node) in args.iter().zip(arg_nodes.iter().cloned()) {
                let param_ix = view.ident_node(id);
                view.nw.add_dep(arg_node, param_ix, Constraint::Flows(()));
            }
            return ix;
        }
        // Just return a new node for the return value. We will infer return types by looking up
        // this node later and adding dependencies when we encounter a `Return` stmt.
        //
        // TODO: this means we do some duplicate work in rewriting returns in the cfg module.

        // Create a new function.
        let res = self.nw.add_rule(Rule::Var);
        self.nw.add_dep(res, base_node, Constraint::Flows(()));
        self.funcs.insert(key.clone(), res);
        let mut view = View {
            tc: self,
            frame_id: *ident,
            frame_args: key.args,
        };

        // Apply the arguments appropriately:
        for (cfg::Arg { id, .. }, arg_node) in args.iter().zip(arg_nodes.iter().cloned()) {
            let ix = view.ident_node(id);
            view.nw.add_dep(arg_node, ix, Constraint::Flows(()));
        }
        let nodes = cfg.raw_nodes();
        for bb in nodes {
            for stmt in bb.weight.q.iter() {
                view.constrain_stmt(stmt);
            }
        }
        res
    }
}

impl<'b, 'c, 'd> View<'b, 'c, 'd> {
    fn add_builtin_call(&mut self, f: builtins::Function, args: SmallVec<NodeIx>, to: NodeIx) {
        // We want to give feedback the opportunity to influence function arguments based on the
        // return value of the function (this is used in IncMap specifically), but since `to` may
        // be used in other places, we set up an additional node to soak up "return-specific
        // information" for the builtin call.
        let tmp_res = self.nw.add_rule(Rule::Var);
        f.feedback(&args[..], tmp_res, self);
        for arg in args.iter() {
            self.nw
                .call_deps
                .entry(*arg)
                .or_insert(Default::default())
                .push(tmp_res);
        }
        let from = self.nw.base_node;
        self.nw
            .add_dep(from, tmp_res, Constraint::CallBuiltin(args, f));
        self.nw.add_dep(tmp_res, to, Constraint::Flows(()));
        self.nw.wl.insert(tmp_res);
        self.nw.wl.insert(to);
    }
    fn add_udf_call(&mut self, f: NumTy, args: SmallVec<NodeIx>, to: NodeIx) {
        for arg in args.iter() {
            self.nw
                .call_deps
                .entry(*arg)
                .or_insert(Default::default())
                .push(to);
        }
        let from = self.udf_nodes[f as usize];
        self.nw
            .add_dep(from, to, Constraint::CallUDF(from, args, f));
        self.nw.wl.insert(to);
    }

    fn constrain_stmt(&mut self, stmt: &cfg::PrimStmt) {
        use cfg::PrimStmt::*;
        match stmt {
            AsgnIndex(arr, ix, v) => {
                let arr_ix = self.ident_node(arr);
                let ix_ix = self.val_node(ix);
                self.constrain_as_map(arr_ix);
                // TODO: set up caching for keys, values of maps and iterators?
                self.nw.add_dep(ix_ix, arr_ix, Constraint::KeyIn(()));
                let val_ix = self.nw.add_rule(Rule::Var);
                self.nw.add_dep(val_ix, arr_ix, Constraint::ValIn(()));
                self.constrain_expr(v, val_ix);
            }
            AsgnVar(v, e) => {
                let v_ix = self.ident_node(v);
                self.constrain_expr(e, v_ix);
            }
            Return(v) => {
                let v_ix = self.val_node(v);
                let cur_func_key = Args {
                    id: self.frame_id,
                    func_id: None,
                    args: self.frame_args.clone(),
                };
                let ret_ix = self.tc.funcs[&cur_func_key];
                self.nw.add_dep(v_ix, ret_ix, Constraint::Flows(()));
            }
            Printf(fmt, args, out) => self.printf_internal(Some(fmt), &args[..], out),
            PrintAll(args, out) => self.printf_internal(None, &args[..], out),
            // Builtins have fixed types; no constraint generation is necessary.
            // For IterDrop, we do not add extra constraints because IterBegin and IterNext will be
            // sufficient to determine the type of a given iterator.
            IterDrop(_) | SetBuiltin(_, _) => {}
        }
    }

    /// A short helper for generating type constraints for `Printf` and `PrintAll`.
    fn printf_internal<'a>(
        &mut self,
        fmt: Option<&cfg::PrimVal<'a>>,
        args: &[cfg::PrimVal<'a>],
        out: &Option<(cfg::PrimVal<'a>, FileSpec)>,
    ) {
        // Printf's arguments can be any scalar.
        //
        // NB: This information isn't really needed for inference, but it simplifies some
        // of the logic when we compile to bytecode to ensure that there is _some_
        // constraint for every variable that shows up in a program. It also helps to
        // localize type errors to this module
        let is_scalar: State = Some(TVar::Scalar(None));
        let scalar_node = self.constant(is_scalar);
        if let Some(fmt) = fmt {
            let fmt_node = self.val_node(fmt);
            self.nw
                .add_dep(scalar_node, fmt_node, Constraint::Flows(()));
        }
        for a in args.iter() {
            let arg_node = self.val_node(a);
            self.nw
                .add_dep(scalar_node, arg_node, Constraint::Flows(()));
        }
        if let Some((out, _)) = out {
            let out_node = self.val_node(out);
            self.nw
                .add_dep(scalar_node, out_node, Constraint::Flows(()));
        }
    }

    fn constrain_expr(&mut self, expr: &cfg::PrimExpr, to: NodeIx) {
        use cfg::PrimExpr::*;
        match expr {
            Val(pv) => {
                let pv_ix = self.val_node(pv);
                self.nw.add_dep(pv_ix, to, Constraint::Flows(()));
            }
            Phi(preds) => {
                for (_, id) in preds.iter() {
                    let id_ix = self.ident_node(id);
                    self.nw.add_dep(id_ix, to, Constraint::Flows(()));
                }
            }
            CallBuiltin(f, args) => {
                let args: SmallVec<NodeIx> = args.iter().map(|arg| self.val_node(arg)).collect();
                self.add_builtin_call(*f, args, to);
            }
            CallUDF(f, args) => {
                let args: SmallVec<NodeIx> = args.iter().map(|arg| self.val_node(arg)).collect();
                self.add_udf_call(*f, args, to);
            }
            Sprintf(fmt, args) => {
                let str_node = self.constant(TVar::Scalar(BaseTy::Str).abs());
                let is_scalar: State = Some(TVar::Scalar(None));
                let scalar_node = self.constant(is_scalar);
                for a in args {
                    let a_node = self.val_node(a);
                    self.nw.add_dep(scalar_node, a_node, Constraint::Flows(()));
                }
                let fmt_node = self.val_node(fmt);
                self.nw
                    .add_dep(scalar_node, fmt_node, Constraint::Flows(()));
                self.nw.add_dep(str_node, to, Constraint::Flows(()));
            }
            Index(arr, ix) => {
                let arr_ix = self.val_node(arr);
                let ix_ix = self.val_node(ix);
                self.constrain_as_map(arr_ix);
                // Referencing `a[k]` creates the element in awk, so the index type also flows
                // into the map's key type (as in AsgnIndex).
                self.nw.add_dep(ix_ix, arr_ix, Constraint::KeyIn(()));
                self.nw.add_dep(arr_ix, ix_ix, Constraint::Key(()));
                self.nw.add_dep(arr_ix, to, Constraint::Val(()));
            }
            IterBegin(arr) => {
                let arr_ix = self.val_node(arr);
                self.constrain_as_map(arr_ix);
                let iter_ix = to;
                let key_ix = self.nw.add_rule(Rule::Var);

                // The `key_ix` is a proxy node that has a bidirectional constraint with the key of
                // the array and the value of the iterator. Its presence ensures that the
                // iterator's type and the map's key type remain the same.
                self.nw.add_dep(key_ix, arr_ix, Constraint::KeyIn(()));
                self.nw.add_dep(arr_ix, key_ix, Constraint::Key(()));
                self.nw.add_dep(key_ix, iter_ix, Constraint::IterValIn(()));
                self.nw.add_dep(iter_ix, key_ix, Constraint::IterVal(()));
            }
            HasNext(_) => {
                let int = self.constant(TVar::Scalar(BaseTy::Int).abs());
                self.nw.add_dep(int, to, Constraint::Flows(()))
            }
            Next(iter) => {
                let iter_ix = self.val_node(iter);
                self.nw.add_dep(iter_ix, to, Constraint::IterVal(()));
            }
            LoadBuiltin(bv) => {
                let bv_ix = self.constant(bv.ty().abs());
                self.nw.add_dep(bv_ix, to, Constraint::Flows(()));
            }
        };
    }

    fn val_node(&mut self, val: &cfg::PrimVal) -> NodeIx {
        use cfg::PrimVal::*;
        match val {
            Var(id) => self.ident_node(id),
            ILit(_) => self.constant(TVar::Scalar(BaseTy::Int).abs()),
            FLit(_) => self.constant(TVar::Scalar(BaseTy::Float).abs()),
            StrLit(_) => self.constant(TVar::Scalar(BaseTy::Str).abs()),
        }
    }

    fn is_global(&self, id: &Ident) -> bool {
        id.is_global(self.local_globals)
    }

    fn ident_node(&mut self, id: &Ident) -> NodeIx {
        // The placeholder variable receives discarded values (e.g. of `r = f()` in a function
        // body) of every kind. Its SSA versions are numbered per function, so keying it globally
        // would merge unrelated versions from different functions into one node.
        let is_global = self.is_global(id) && !is_unused(*id);
        let key = Args {
            id: *id,
            func_id: if is_global { None } else { Some(self.frame_id) },
            args: if is_global {
                Default::default()
            } else {
                self.frame_args.clone()
            },
        };
        self.get_node(key)
    }
}
