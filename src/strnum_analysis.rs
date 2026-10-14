//! A static analysis of which strings may be "strnums".
//!
//! In awk, strings that come from input (fields, getline, split, ARGV, ENVIRON, -v assignments)
//! compare numerically when they look like numbers, while other strings (constants, results of
//! concatenation and string functions) always compare as strings (see `runtime::compare`).
//! Comparisons with string operands are compiled to `CmpStr`/`CmpStrNum` instructions that assume
//! every string may be a strnum; this analysis finds the operands that cannot be, so that they are
//! compared as strings. This is what makes e.g. `$1 "" < $2 ""` compare as strings.
//!
//! Unlike taint analysis, "strnum-ness" only flows through copies: assignments, phi nodes, array
//! stores and loads, global variables, slots, function arguments and returns. Any other operation
//! produces a plain string. The analysis is conservative where it is imprecise: numbers converted
//! to strings are assumed to be strnums (such values compare numerically when they look like
//! numbers, as numbers do), and array keys are plain strings, as in gawk.
use crate::builtins::Variable;
use crate::bytecode::Instr;
use crate::common::NumTy;
use crate::cfg::SmallVec;
use crate::compile::{HighLevel, Ty};
use crate::dataflow::{self, JoinSemiLattice, Key};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Strnum {
    No,
    Maybe,
}

impl JoinSemiLattice for Strnum {
    type Func = ();
    fn bottom() -> Strnum {
        Strnum::No
    }
    fn invoke(&mut self, other: &Strnum, (): &()) -> bool /* changed */ {
        if *self == Strnum::No && *other == Strnum::Maybe {
            *self = Strnum::Maybe;
            true
        } else {
            false
        }
    }
}

pub(crate) struct StrnumAnalysis {
    dfa: dataflow::Analysis<Strnum>,
    // The parameter registers of each function, indexed by function id.
    params: Vec<SmallVec<(NumTy, Ty)>>,
}

impl Default for StrnumAnalysis {
    fn default() -> StrnumAnalysis {
        let mut dfa = dataflow::Analysis::default();
        dfa.add_src(Key::VarVal(Variable::ARGV), Strnum::Maybe);
        dfa.add_src(Key::VarVal(Variable::ENVIRON), Strnum::Maybe);
        StrnumAnalysis {
            dfa,
            params: Vec::new(),
        }
    }
}

impl StrnumAnalysis {
    /// Mark a string register as possibly holding a strnum.
    pub(crate) fn add_source(&mut self, reg: NumTy, ty: Ty) {
        self.dfa.add_src(Key::Reg(reg, ty), Strnum::Maybe);
    }

    /// Set the parameter registers (and their types) of every function, indexed by function id.
    pub(crate) fn set_params(&mut self, params: Vec<SmallVec<(NumTy, Ty)>>) {
        self.params = params;
    }

    pub(crate) fn visit_hl(&mut self, cur_fn_id: NumTy, inst: &HighLevel) {
        use HighLevel::*;
        match inst {
            // Return values flow to the call and string arguments flow into the parameters.
            // Arrays are passed by reference, so the values of array arguments and parameters
            // flow both ways.
            Call {
                func_id,
                dst_reg,
                dst_ty,
                args,
            } => {
                self.dfa
                    .add_dep(Key::Reg(*dst_reg, *dst_ty), Key::Func(*func_id), ());
                let params = &self.params[*func_id as usize];
                for (&(arg, arg_ty), &(param, ty)) in args.iter().zip(params.iter()) {
                    debug_assert_eq!(arg_ty, ty);
                    if ty == Ty::Str {
                        self.dfa.add_dep(Key::Reg(param, ty), Key::Reg(arg, ty), ());
                    } else if ty.is_array() && ty.val().ok() == Some(Ty::Str) {
                        self.dfa.add_dep(Key::MapVal(param, ty), Key::MapVal(arg, ty), ());
                        self.dfa.add_dep(Key::MapVal(arg, ty), Key::MapVal(param, ty), ());
                    }
                }
            }
            Ret(..) | Phi(..) => dataflow::boilerplate::visit_hl(inst, cur_fn_id, |dst, src| {
                if let Some(src) = src {
                    self.dfa.add_dep(dst, src, ())
                }
            }),
            DropIter(..) => {}
        }
    }

    pub(crate) fn visit_ll(&mut self, inst: &Instr) {
        use Instr::*;
        match inst {
            GetColumn(dst, _) | NextLine(dst, _, _) | NextLineStdin(dst) | IntToStr(dst, _)
            | FloatToStr(dst, _) => self.dfa.add_src(dst, Strnum::Maybe),
            SplitInt(_, _, map, _) => self.add_map_vals(map),
            SplitStr(_, _, map, _) => self.add_map_vals(map),
            SplitSeps { arr_ty, arr, seps_ty, seps, .. } => {
                self.dfa.add_src(Key::MapVal(*arr, *arr_ty), Strnum::Maybe);
                self.dfa.add_src(Key::MapVal(*seps, *seps_ty), Strnum::Maybe);
            }
            MatchArrInt(_, _, _, map) => self.add_map_vals(map),
            MatchArrStr(_, _, _, map) => self.add_map_vals(map),
            CmpStr { l, r, .. } => {
                self.dfa.add_query(l);
                self.dfa.add_query(r);
            }
            CmpStrNum { s, .. } => self.dfa.add_query(s),
            // The values of `src` flow into `dst`.
            Asort { src_ty, src, dst_ty, dst, .. } if src_ty.val().ok() == Some(Ty::Str) => {
                self.dfa.add_query(Key::MapVal(*src, *src_ty));
                self.dfa
                    .add_dep(Key::MapVal(*dst, *dst_ty), Key::MapVal(*src, *src_ty), ());
            }
            AwkStrtonum(_, s, _) | NotStr(_, s, _) | TypeOfString(_, s, _) => {
                self.dfa.add_query(s)
            }
            Printf { args, .. } | Sprintf { args, .. } => {
                for (reg, ty) in args.iter().cloned() {
                    if ty == Ty::Str {
                        self.dfa.add_query(Key::Reg(reg, ty));
                    }
                }
            }
            // Copies.
            Mov(..)
            | Lookup { .. }
            | Store { .. }
            | LoadVarStr(..)
            | StoreVarStr(..)
            | LoadVarIntMap(..)
            | StoreVarIntMap(..)
            | LoadVarStrMap(..)
            | StoreVarStrMap(..)
            | LoadVarStrStrMap(..)
            | StoreVarStrStrMap(..)
            | LoadSlot { .. }
            | StoreSlot { .. } => dataflow::boilerplate::visit_ll(inst, |dst, src| {
                if let Some(src) = src {
                    self.dfa.add_dep(dst, src, ())
                }
            }),
            _ => {}
        }
    }

    fn add_map_vals<T>(&mut self, map: &crate::bytecode::Reg<T>)
    where
        crate::bytecode::Reg<T>: crate::bytecode::Accum,
    {
        use crate::bytecode::Accum;
        let (reg, ty) = map.reflect();
        self.dfa.add_src(Key::MapVal(reg, ty), Strnum::Maybe);
    }

    /// Whether the string register `reg` may hold a strnum. `reg` must have been queried, i.e. be
    /// an operand of a `CmpStr`, `CmpStrNum`, `AwkStrtonum`, `NotStr` or `TypeOfString` instruction, or a string
    /// argument of a `Printf` or `Sprintf` instruction, visited by `visit_ll`.
    pub(crate) fn may_be_strnum(&mut self, reg: NumTy) -> bool {
        *self.dfa.query(Key::Reg(reg, Ty::Str)) == Strnum::Maybe
    }

    /// Whether the string-valued array `map` may hold strnums. `map` must be the source of an
    /// `Asort` instruction visited by `visit_ll`.
    pub(crate) fn map_may_have_strnums(&mut self, map: NumTy, ty: Ty) -> bool {
        *self.dfa.query(Key::MapVal(map, ty)) == Strnum::Maybe
    }
}
