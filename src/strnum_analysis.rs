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
//! stores and loads, global variables, slots and function returns. Any other operation produces a
//! plain string. The analysis is conservative where it is imprecise: function parameters and
//! numbers converted to strings are assumed to be strnums (such values compare numerically when
//! they look like numbers, as numbers do), and array keys are plain strings, as in gawk.
use crate::builtins::Variable;
use crate::bytecode::Instr;
use crate::common::NumTy;
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
}

impl Default for StrnumAnalysis {
    fn default() -> StrnumAnalysis {
        let mut dfa = dataflow::Analysis::default();
        dfa.add_src(Key::VarVal(Variable::ARGV), Strnum::Maybe);
        dfa.add_src(Key::VarVal(Variable::ENVIRON), Strnum::Maybe);
        StrnumAnalysis { dfa }
    }
}

impl StrnumAnalysis {
    /// Mark a register (e.g. a function parameter) as possibly holding a strnum.
    pub(crate) fn add_source(&mut self, reg: NumTy, ty: Ty) {
        self.dfa.add_src(Key::Reg(reg, ty), Strnum::Maybe);
    }

    pub(crate) fn visit_hl(&mut self, cur_fn_id: NumTy, inst: &HighLevel) {
        use HighLevel::*;
        match inst {
            // Return values flow to the call; arguments only flow into parameters, which are
            // sources already.
            Call {
                func_id,
                dst_reg,
                dst_ty,
                ..
            } => self
                .dfa
                .add_dep(Key::Reg(*dst_reg, *dst_ty), Key::Func(*func_id), ()),
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
            MatchArrInt(_, _, _, map) => self.add_map_vals(map),
            MatchArrStr(_, _, _, map) => self.add_map_vals(map),
            CmpStr { l, r, .. } => {
                self.dfa.add_query(l);
                self.dfa.add_query(r);
            }
            CmpStrNum { s, .. } => self.dfa.add_query(s),
            AwkStrtonum(_, s, _) | NotStr(_, s, _) => self.dfa.add_query(s),
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
    /// an operand of a `CmpStr`, `CmpStrNum`, `AwkStrtonum` or `NotStr` instruction visited by
    /// `visit_ll`.
    pub(crate) fn may_be_strnum(&mut self, reg: NumTy) -> bool {
        *self.dfa.query(Key::Reg(reg, Ty::Str)) == Strnum::Maybe
    }
}
