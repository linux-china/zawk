//! The `codegen` module provides general tools for implementing different backends for frawk
//! programs based on the output of the `compile` module.
//!
//! The module root contains code that is shared by the cranelift and LLVM backends.
use crate::{
    builtins,
    bytecode::{self, Accum},
    common::{CancelSignal, Cleanup, FileSpec, NumTy, Result, Stage},
    compile,
    pushdown::FieldSet,
    runtime::{self, UniqueStr},
};

use regex::bytes::Regex;

use std::marker::PhantomData;
use std::mem;
use std::sync::Arc;

/// Options used to configure a code-generating backend.
#[derive(Copy, Clone)]
pub struct Config {
    pub opt_level: usize,
    pub num_workers: usize,
}

macro_rules! external {
    ($name:ident) => {
        crate::codegen::intrinsics::$name as *const u8
    };
}

#[macro_use]
pub(crate) mod intrinsics;
pub(crate) mod clif;
#[cfg(feature = "llvm_backend")]
pub(crate) mod llvm;

use intrinsics::Runtime;

pub(crate) type Ref = (NumTy, compile::Ty);
pub(crate) type StrReg<'a> = bytecode::Reg<runtime::Str<'a>>;

pub(crate) struct Sig<'a, C: Backend + ?Sized> {
    #[allow(dead_code)]
    pub attrs: &'a [FunctionAttr],
    pub args: &'a mut [C::Ty],
    pub ret: Option<C::Ty>,
}

macro_rules! intrinsic {
    ($name:ident) => {
        Op::Intrinsic(external!($name))
    };
}

#[derive(Copy, Clone)]
pub(crate) enum Cmp {
    Eq,
    Lte,
    Lt,
    Gte,
    Gt,
}

pub(crate) enum Arith {
    Mul,
    Minus,
    Add,
    Mod,
    Neg,
}

pub(crate) enum Op {
    // TODO: we probably don't need the is_float here? we could infer based on the operands?
    // otoh it's more explicit...
    Cmp { is_float: bool, op: Cmp },
    Arith { is_float: bool, op: Arith },
    Bitwise(builtins::Bitwise),
    Math(builtins::FloatFunc),
    Div,
    Pow,
    FloatToInt,
    IntToFloat,
    Intrinsic(*const u8),
}

fn op(op: Arith, is_float: bool) -> Op {
    Op::Arith { is_float, op }
}
fn cmp(op: Cmp, is_float: bool) -> Op {
    Op::Cmp { is_float, op }
}

#[derive(Debug, Copy, Clone)]
pub enum FunctionAttr {
    ReadOnly,
    ArgmemOnly,
}

/// A handle around a generated main function, potentially allocated dynamically with the given
/// lifetime.
#[derive(Copy, Clone)]
pub(crate) struct MainFunction<'a> {
    fn_ptr: *const u8,
    _marker: PhantomData<&'a ()>,
}

unsafe impl<'a> Send for MainFunction<'a> {}

impl<'a> MainFunction<'a> {
    /// Create a `MainFunction` of arbitrary lifetime.
    ///
    /// This is only called from the default impl in the `Jit` trait. General use is extra unsafe
    /// as it allows the caller to "cook up" whatever lifetime suits them.
    fn from_ptr(fn_ptr: *const u8) -> MainFunction<'a> {
        MainFunction {
            fn_ptr,
            _marker: PhantomData,
        }
    }

    /// Call the generated function on the given runtime.
    ///
    /// Unsafe because it pretends an arbitrary memory location contains code, and then runs that
    /// code.
    pub(crate) unsafe fn invoke<'b>(&self, rt: &mut Runtime<'b>) {
        mem::transmute::<*const u8, unsafe extern "C" fn(*mut Runtime<'b>)>(self.fn_ptr)(rt)
    }
}

pub(crate) trait Jit: Sized {
    fn main_pointers(&mut self) -> Result<Stage<*const u8>>;
    fn main_functions(&mut self) -> Result<Stage<MainFunction>> {
        Ok(self.main_pointers()?.map(MainFunction::from_ptr))
    }
}

/// Run the main function (or functions, for parallel scripts) given a [`Jit`] and the various
/// other parameters required to construct a runtime.
pub(crate) unsafe fn run_main<R, FF, J>(
    mut jit: J,
    stdin: R,
    ff: FF,
    used_fields: &FieldSet,
    named_columns: Option<Vec<&[u8]>>,
    num_workers: usize,
    cancel_signal: CancelSignal,
) -> Result<()>
where
    R: intrinsics::IntoRuntime,
    FF: runtime::writers::FileFactory,
    J: Jit,
{
    let mut rt = stdin.into_runtime(ff, used_fields, named_columns, cancel_signal.clone());
    let main = jit.main_functions()?;
    match main {
        Stage::Main(m) => {
            m.invoke(&mut rt);
            Ok(())
        }
        Stage::Par {
            begin,
            main_loop,
            end,
        } => {
            // This triply-nested macro is here to allow mutable access to a "runtime" struct
            // as well as mutable access to the same "read_files" value. The generated code is
            // pretty awful; It may be worth a RefCell just to clean up.
            with_input!(&mut rt.input_data, |(_, read_files)| {
                let reads = read_files.try_resize(num_workers.saturating_sub(1));
                if num_workers <= 1 || reads.is_empty() || main_loop.is_none() {
                    // execute serially.
                    for main in begin.into_iter().chain(main_loop).chain(end) {
                        main.invoke(&mut rt);
                    }
                    return Ok(());
                }
                #[cfg(not(debug_assertions))]
                {
                    std::panic::set_hook(Box::new(|pi| {
                        if let Some(s) = pi.payload().downcast_ref::<&str>() {
                            if s.len() > 0 {
                                eprintln_ignore!("{}", s);
                            }
                        }
                    }));
                }
                if let Some(begin) = begin {
                    begin.invoke(&mut rt);
                }
                if rt.core.write_files.flush_stdout().is_err() {
                    return Ok(());
                }

                rt.concurrent = true;

                let (sender, receiver) = crossbeam_channel::bounded(reads.len());
                let launch_data: Vec<_> = reads
                    .into_iter()
                    .enumerate()
                    .map(|(i, reader)| {
                        (
                            reader,
                            sender.clone(),
                            rt.core.shuttle(i as runtime::Int + 2),
                        )
                    })
                    .collect();
                with_input!(&mut rt.input_data, |(_, read_files)| {
                    let old_read_files = mem::take(&mut read_files.inputs);
                    let main_loop_fn = main_loop.unwrap();
                    let scope_res = crossbeam::scope(|s| {
                        for (reader, sender, shuttle) in launch_data.into_iter() {
                            let cancel_signal = cancel_signal.clone();
                            s.spawn(move |_| {
                                if let Some(reader) = reader() {
                                    let mut runtime = Runtime {
                                        concurrent: true,
                                        core: shuttle(),
                                        input_data: reader.into(),
                                        cleanup: Cleanup::<Runtime>::new(move |rt| {
                                            sender.send(rt.core.extract_result(0)).unwrap();
                                        }),
                                        cancel_signal,
                                    };
                                    main_loop_fn.invoke(&mut runtime);
                                }
                            });
                        }
                        mem::drop(sender);
                        {
                            rt.core.vars.pid = 1;
                            let r = receiver.clone();
                            rt.cleanup =
                                Cleanup::<Runtime>::new(move |_| while r.recv().is_ok() {});
                            main_loop_fn.invoke(&mut rt);
                            rt.cleanup.cancel();
                        }
                        rt.core.vars.pid = 0;

                        with_input!(&mut rt.input_data, |(_, read_files)| {
                            while let Ok(res) = receiver.recv() {
                                rt.core.combine(res);
                            }
                            if let Some(rc) = cancel_signal.get_code() {
                                mem::drop(rt);
                                std::process::exit(rc);
                            }
                            rt.concurrent = false;
                            if let Some(end) = end {
                                read_files.inputs = old_read_files;
                                end.invoke(&mut rt);
                            }
                        });
                    });
                    if scope_res.is_err() {
                        return err!("failed to execute parallel script");
                    }
                });
            });
            Ok(())
        }
    }
}

/// Handles to ensure the liveness of rust objects passed by pointer into generated code.
#[derive(Default)]
pub(crate) struct Handles {
    res: Vec<Arc<Regex>>,
    slices: Vec<Arc<[u8]>>,
}

pub(crate) trait Backend {
    type Ty: Clone;
    // mappings from compile::Ty to Self::Ty
    fn void_ptr_ty(&self) -> Self::Ty;
    fn ptr_to(&self, ty: Self::Ty) -> Self::Ty;
    fn usize_ty(&self) -> Self::Ty;
    fn u32_ty(&self) -> Self::Ty;
    fn get_ty(&self, ty: compile::Ty) -> Self::Ty;

    /// Register a function with address `addr` and name `name` (/ `name_c`, the nul-terminated
    /// variant if needed) with signature `Sig` to be called.
    fn register_external_fn(
        &mut self,
        name: &'static str,
        name_c: *const u8,
        addr: *const u8,
        sig: Sig<Self>,
    ) -> Result<()>;
}

/// CodeGenerator encapsulates common functionality needed to generate instructions across multiple
/// backends. This trait is not currently sufficient to abstract over any backend "end to end" from
/// bytecode instructions all the way to machine code, but it allows us to keep much of the more
/// mundane plumbing work common across all backends (as well as separate safe "glue code" from
/// unsafe calls to the LLVM C API).
pub(crate) trait CodeGenerator: Backend {
    type Val;

    // mappings to and from bytecode-level registers to IR-level values
    fn bind_val(&mut self, r: Ref, v: Self::Val) -> Result<()>;
    fn get_val(&mut self, r: Ref) -> Result<Self::Val>;

    // backend-specific handling of constants and low-level operations.
    fn runtime_val(&mut self) -> Self::Val;
    fn const_int(&mut self, i: i64) -> Self::Val;
    fn const_float(&mut self, f: f64) -> Self::Val;
    fn const_str(&mut self, s: &UniqueStr) -> Self::Val;

    // const ptr should not be called directly.
    fn const_ptr<T>(&mut self, r: *const T) -> Self::Val;
    fn handles(&mut self) -> &mut Handles;

    // const_{re,slice} take an `Arc` so that it can store a pointer to `c` and ensure references
    // to `c` will live as long as the generated code.

    fn const_re(&mut self, pat: Arc<Regex>) -> Self::Val {
        let res = self.const_ptr(&*pat);
        self.handles().res.push(pat);
        res
    }
    fn const_slice(&mut self, bs: Arc<[u8]>) -> Self::Val {
        let res = self.const_ptr(bs.as_ptr());
        self.handles().slices.push(bs);
        res
    }

    // NB: why &mut [..] everywhere instead of &[..] or impl Iterator<..>? The LLVM C API takes a
    // sequence of arguments by a mutable pointer to the first element along with a length. We
    // could take `args.as_ptr() as *mut _`, as most of these LLVM calls (probably?) don't actually
    // modify the list, but we may as well err on the safe side and behave as though LLVM has
    // mutable access to the contents of the slice.

    /// Call an intrinsic, given a pointer to the [`intrinsics`] module and a list of arguments.
    fn call_intrinsic(&mut self, func: Op, args: &mut [Self::Val]) -> Result<Self::Val>;

    /// Call an external function that does not return a value.
    ///
    /// Some backends (LLVM) are fine with returning a "value" with no content; for that case we
    /// simply delegate to [`call_intrinsic`]. However, in cranelift "void" functions simply do not
    /// return a value. We could add a wrapper value type that permits "no value" as a member, but
    /// then the rest of the code would have to have unwrap's everywhere (making the code less
    /// clear and less type safe).
    ///
    /// [`call_intrinsic`]:[crate::codegen::CodeGenerator::call_intrinsic]
    fn call_void(&mut self, func: *const u8, args: &mut [Self::Val]) -> Result<()> {
        self.call_intrinsic(Op::Intrinsic(func), args)?;
        Ok(())
    }

    // var-arg printing functions. The arguments here directly parallel the instruction
    // definitions.

    fn printf(
        &mut self,
        output: &Option<(StrReg, FileSpec)>,
        fmt: &StrReg,
        args: &[Ref],
    ) -> Result<()>;

    fn sprintf(&mut self, dst: &StrReg, fmt: &StrReg, args: &[Ref]) -> Result<()>;

    fn print_all(&mut self, output: &Option<(StrReg, FileSpec)>, args: &[StrReg]) -> Result<()>;

    /// Moves the contents of `src` into `dst`, taking refcounts into consideration if necessary.
    fn mov(&mut self, ty: compile::Ty, dst: NumTy, src: NumTy) -> Result<()>;

    /// Constructs an iterator over the keys of `map` and stores it in `dst`.
    fn iter_begin(&mut self, dst: Ref, map: Ref) -> Result<()>;

    /// Queries the iterator in `iter` as to whether any elements remain, stores the result in the
    /// `dst` register.
    fn iter_hasnext(&mut self, dst: Ref, iter: Ref) -> Result<()>;

    /// Advances the iterator in `iter` to the next element and stores the current element in `dst`
    fn iter_getnext(&mut self, dst: Ref, iter: Ref) -> Result<()>;

    // derived functions

    /// Loads contents of given slot into dst.
    ///
    /// Assumes that dst.1 is a type we can store in a slot (i.e. it cannot be an iterator)
    fn load_slot(&mut self, dst: Ref, slot: i64) -> Result<()> {
        use compile::Ty::*;
        let slot_v = self.const_int(slot);
        let func = match dst.1 {
            Int => intrinsic!(load_slot_int),
            Float => intrinsic!(load_slot_float),
            Str => intrinsic!(load_slot_str),
            MapIntInt => intrinsic!(load_slot_intint),
            MapIntFloat => intrinsic!(load_slot_intfloat),
            MapIntStr => intrinsic!(load_slot_intstr),
            MapStrInt => intrinsic!(load_slot_strint),
            MapStrFloat => intrinsic!(load_slot_strfloat),
            MapStrStr => intrinsic!(load_slot_strstr),
            _ => unreachable!(),
        };
        let rt = self.runtime_val();
        let resv = self.call_intrinsic(func, &mut [rt, slot_v])?;
        self.bind_val(dst, resv)
    }

    /// Stores contents of src into a given slot.
    ///
    /// Assumes that src.1 is a type we can store in a slot (i.e. it cannot be an iterator)
    fn store_slot(&mut self, src: Ref, slot: i64) -> Result<()> {
        use compile::Ty::*;
        let slot_v = self.const_int(slot);
        let func = match src.1 {
            Int => external!(store_slot_int),
            Float => external!(store_slot_float),
            Str => external!(store_slot_str),
            MapIntInt => external!(store_slot_intint),
            MapIntFloat => external!(store_slot_intfloat),
            MapIntStr => external!(store_slot_intstr),
            MapStrInt => external!(store_slot_strint),
            MapStrFloat => external!(store_slot_strfloat),
            MapStrStr => external!(store_slot_strstr),
            _ => unreachable!(),
        };
        let rt = self.runtime_val();
        let arg = self.get_val(src)?;
        self.call_void(func, &mut [rt, slot_v, arg])?;
        Ok(())
    }

    /// Retrieves the contents of `map` at `key` and stores them in `dst`.
    ///
    /// These are "awk lookups" that insert a default value into the map if it is not presetn.
    /// Assumes that types of map, key, dst match up.
    fn lookup_map(&mut self, map: Ref, key: Ref, dst: Ref) -> Result<()> {
        use compile::Ty::*;
        map_valid(map.1, key.1, dst.1)?;
        let func = match map.1 {
            MapIntInt => intrinsic!(lookup_intint),
            MapIntFloat => intrinsic!(lookup_intfloat),
            MapIntStr => intrinsic!(lookup_intstr),
            MapStrInt => intrinsic!(lookup_strint),
            MapStrFloat => intrinsic!(lookup_strfloat),
            MapStrStr => intrinsic!(lookup_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        let keyv = self.get_val(key)?;
        let resv = self.call_intrinsic(func, &mut [mapv, keyv])?;
        self.bind_val(dst, resv)?;
        Ok(())
    }

    /// Deletes the contents of `map` at `key`.
    ///
    /// Assumes that map and key types match up.
    fn delete_map(&mut self, map: Ref, key: Ref) -> Result<()> {
        use compile::Ty::*;
        map_key_valid(map.1, key.1)?;
        let func = match map.1 {
            MapIntInt => external!(delete_intint),
            MapIntFloat => external!(delete_intfloat),
            MapIntStr => external!(delete_intstr),
            MapStrInt => external!(delete_strint),
            MapStrFloat => external!(delete_strfloat),
            MapStrStr => external!(delete_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        let keyv = self.get_val(key)?;
        self.call_void(func, &mut [mapv, keyv])?;
        Ok(())
    }

    /// Deletes the contents of `map`.
    ///
    /// Assumes that map and key types match up.
    fn clear_map(&mut self, map: Ref) -> Result<()> {
        use compile::Ty::*;
        let func = match map.1 {
            MapIntInt => external!(clear_intint),
            MapIntFloat => external!(clear_intfloat),
            MapIntStr => external!(clear_intstr),
            MapStrInt => external!(clear_strint),
            MapStrFloat => external!(clear_strfloat),
            MapStrStr => external!(clear_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        self.call_void(func, &mut [mapv])?;
        Ok(())
    }

    /// Determines if `map` contains `key` and stores the result (0 or 1) in `dst`.
    ///
    /// Assumes that map and key types match up.
    fn contains_map(&mut self, map: Ref, key: Ref, dst: Ref) -> Result<()> {
        use compile::Ty::*;
        map_key_valid(map.1, key.1)?;
        let func = match map.1 {
            MapIntInt => intrinsic!(contains_intint),
            MapIntFloat => intrinsic!(contains_intfloat),
            MapIntStr => intrinsic!(contains_intstr),
            MapStrInt => intrinsic!(contains_strint),
            MapStrFloat => intrinsic!(contains_strfloat),
            MapStrStr => intrinsic!(contains_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        let keyv = self.get_val(key)?;
        let resv = self.call_intrinsic(func, &mut [mapv, keyv])?;
        self.bind_val(dst, resv)?;
        Ok(())
    }

    /// Stores the size of `map` in `dst`.
    fn len_map(&mut self, map: Ref, dst: Ref) -> Result<()> {
        use compile::Ty::*;
        let func = match map.1 {
            MapIntInt => intrinsic!(len_intint),
            MapIntFloat => intrinsic!(len_intfloat),
            MapIntStr => intrinsic!(len_intstr),
            MapStrInt => intrinsic!(len_strint),
            MapStrFloat => intrinsic!(len_strfloat),
            MapStrStr => intrinsic!(len_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        let resv = self.call_intrinsic(func, &mut [mapv])?;
        self.bind_val(dst, resv)?;
        Ok(())
    }

    /// Stores `val` into `map` at key `key`.
    ///
    /// Assumes that the types of the input registers match up.
    fn store_map(&mut self, map: Ref, key: Ref, val: Ref) -> Result<()> {
        use compile::Ty::*;
        map_valid(map.1, key.1, val.1)?;
        let func = match map.1 {
            MapIntInt => external!(insert_intint),
            MapIntFloat => external!(insert_intfloat),
            MapIntStr => external!(insert_intstr),
            MapStrInt => external!(insert_strint),
            MapStrFloat => external!(insert_strfloat),
            MapStrStr => external!(insert_strstr),
            ty => return err!("non-map type: {:?}", ty),
        };
        let mapv = self.get_val(map)?;
        let keyv = self.get_val(key)?;
        let valv = self.get_val(val)?;
        self.call_void(func, &mut [mapv, keyv, valv])?;
        Ok(())
    }

    /// Increments the value in `map` at `key` (inserting a default value if necessary) by `by`
    /// (either a float or int register), storing a copy of the value in `dst`.
    fn inc_map(&mut self, map: Ref, key: Ref, by: Ref, dst: Ref) -> Result<()> {
        use compile::Ty::*;
        map_valid(map.1, key.1, dst.1)?;
        let func = match (map.1, by.1) {
            (MapIntInt, Int) => intrinsic!(inc_int_intint),
            (MapIntFloat, Int) => intrinsic!(inc_int_intfloat),
            (MapIntStr, Int) => intrinsic!(inc_int_intstr),
            (MapStrInt, Int) => intrinsic!(inc_int_strint),
            (MapStrFloat, Int) => intrinsic!(inc_int_strfloat),
            (MapStrStr, Int) => intrinsic!(inc_int_strstr),
            (MapIntInt, Float) => intrinsic!(inc_float_intint),
            (MapIntFloat, Float) => intrinsic!(inc_float_intfloat),
            (MapIntStr, Float) => intrinsic!(inc_float_intstr),
            (MapStrInt, Float) => intrinsic!(inc_float_strint),
            (MapStrFloat, Float) => intrinsic!(inc_float_strfloat),
            (MapStrStr, Float) => intrinsic!(inc_float_strstr),
            (_, x) => return err!("invalid increment type passed to inc_map: {:?}", x),
        };
        let mapv = self.get_val(map)?;
        let keyv = self.get_val(key)?;
        let byv = self.get_val(by)?;
        let resv = self.call_intrinsic(func, &mut [mapv, keyv, byv])?;
        self.bind_val(dst, resv)
    }

    /// Wraps `call_intrinsic` for [`Op`]s that have two arguments and return a value.
    fn binop(&mut self, op: Op, dst: &impl Accum, l: &impl Accum, r: &impl Accum) -> Result<()> {
        let lv = self.get_val(l.reflect())?;
        let rv = self.get_val(r.reflect())?;
        let res = self.call_intrinsic(op, &mut [lv, rv])?;
        self.bind_val(dst.reflect(), res)
    }

    /// Wraps `call_intrinsic` for [`Op`]s that have one argument and return a value.
    fn unop(&mut self, op: Op, dst: &impl Accum, x: &impl Accum) -> Result<()> {
        let xv = self.get_val(x.reflect())?;
        let res = self.call_intrinsic(op, &mut [xv])?;
        self.bind_val(dst.reflect(), res)
    }

    fn gen_ll_inst(&mut self, inst: &compile::LL) -> Result<()> {
        use crate::bytecode::Instr::*;
        match inst {
            StoreConstStr(sr, s) => {
                let sv = self.const_str(s);
                self.bind_val(sr.reflect(), sv)
            }
            StoreConstInt(ir, i) => {
                let iv = self.const_int(*i);
                self.bind_val(ir.reflect(), iv)
            }
            StoreConstFloat(fr, f) => {
                let fv = self.const_float(*f);
                self.bind_val(fr.reflect(), fv)
            }
            IntToStr(sr, ir) => self.unop(intrinsic!(int_to_str), sr, ir),
            FloatToStr(sr, fr) => self.unop(intrinsic!(float_to_str), sr, fr),
            StrToInt(ir, sr) => self.unop(intrinsic!(str_to_int), ir, sr),
            HexStrToInt(ir, sr) => self.unop(intrinsic!(hex_str_to_int), ir, sr),
            StrToFloat(fr, sr) => self.unop(intrinsic!(str_to_float), fr, sr),
            FloatToInt(ir, fr) => self.unop(Op::FloatToInt, ir, fr),
            IntToFloat(fr, ir) => self.unop(Op::IntToFloat, fr, ir),
            ToLowerAscii(dst, src) => self.unop(intrinsic!(to_lower_ascii), dst, src),
            ToUpperAscii(dst, src) => self.unop(intrinsic!(to_upper_ascii), dst, src),
            AddInt(res, l, r) => self.binop(op(Arith::Add, false), res, l, r),
            AddFloat(res, l, r) => self.binop(op(Arith::Add, true), res, l, r),
            MinusInt(res, l, r) => self.binop(op(Arith::Minus, false), res, l, r),
            MinusFloat(res, l, r) => self.binop(op(Arith::Minus, true), res, l, r),
            MulInt(res, l, r) => self.binop(op(Arith::Mul, false), res, l, r),
            MulFloat(res, l, r) => self.binop(op(Arith::Mul, true), res, l, r),
            ModInt(res, l, r) => self.binop(op(Arith::Mod, false), res, l, r),
            ModFloat(res, l, r) => self.binop(op(Arith::Mod, true), res, l, r),
            Div(res, l, r) => self.binop(Op::Div, res, l, r),
            Pow(res, l, r) => self.binop(Op::Pow, res, l, r),
            Not(res, ir) => {
                let iv = self.get_val(ir.reflect())?;
                let zero = self.const_int(0);
                let cmp = self.call_intrinsic(
                    Op::Cmp {
                        is_float: false,
                        op: Cmp::Eq,
                    },
                    &mut [iv, zero],
                )?;
                self.bind_val(res.reflect(), cmp)
            }
            NotStr(res, sr) => {
                let sv = self.get_val(sr.reflect())?;
                let lenv = self.call_intrinsic(intrinsic!(str_len), &mut [sv])?;
                let zero = self.const_int(0);
                let cmp = self.call_intrinsic(
                    Op::Cmp {
                        is_float: false,
                        op: Cmp::Eq,
                    },
                    &mut [lenv, zero],
                )?;
                self.bind_val(res.reflect(), cmp)
            }
            NegInt(res, ir) => self.unop(op(Arith::Neg, false), res, ir),
            NegFloat(res, fr) => self.unop(op(Arith::Neg, true), res, fr),
            Float1(ff, dst, src) => self.unop(Op::Math(*ff), dst, src),
            Float2(ff, dst, l, r) => self.binop(Op::Math(*ff), dst, l, r),
            Int1(bw, dst, src) => self.unop(Op::Bitwise(*bw), dst, src),
            Int2(bw, dst, l, r) => self.binop(Op::Bitwise(*bw), dst, l, r),
            Rand(dst) => {
                let rt = self.runtime_val();
                let res = self.call_intrinsic(intrinsic!(rand_float), &mut [rt])?;
                self.bind_val(dst.reflect(), res)
            }
            Srand(dst, seed) => {
                let rt = self.runtime_val();
                let seedv = self.get_val(seed.reflect())?;
                let res = self.call_intrinsic(intrinsic!(seed_rng), &mut [rt, seedv])?;
                self.bind_val(dst.reflect(), res)
            }
            ReseedRng(dst) => {
                let rt = self.runtime_val();
                let res = self.call_intrinsic(intrinsic!(reseed_rng), &mut [rt])?;
                self.bind_val(dst.reflect(), res)
            }
            Concat(dst, l, r) => self.binop(intrinsic!(concat), dst, l, r),
            StartsWithConst(dst, s, bs) => {
                let s = self.get_val(s.reflect())?;
                let ptr = self.const_slice(bs.clone());
                let len = self.const_int(bs.len() as i64);
                let res = self.call_intrinsic(intrinsic!(starts_with_const), &mut [s, ptr, len])?;
                self.bind_val(dst.reflect(), res)
            }
            Match(dst, l, r) => {
                let lv = self.get_val(l.reflect())?;
                let rv = self.get_val(r.reflect())?;
                let rt = self.runtime_val();
                let res = self.call_intrinsic(intrinsic!(match_pat_loc), &mut [rt, lv, rv])?;
                self.bind_val(dst.reflect(), res)
            }
            IsMatch(dst, l, r) => {
                let lv = self.get_val(l.reflect())?;
                let rv = self.get_val(r.reflect())?;
                let rt = self.runtime_val();
                let res = self.call_intrinsic(intrinsic!(match_pat), &mut [rt, lv, rv])?;
                self.bind_val(dst.reflect(), res)
            }
            MatchConst(res, src, pat) => {
                let rt = self.runtime_val();
                let srcv = self.get_val(src.reflect())?;
                let patv = self.const_re(pat.clone());
                let resv =
                    self.call_intrinsic(intrinsic!(match_const_pat_loc), &mut [rt, srcv, patv])?;
                self.bind_val(res.reflect(), resv)
            }
            IsMatchConst(res, src, pat) => {
                let srcv = self.get_val(src.reflect())?;
                let patv = self.const_re(pat.clone());
                let resv = self.call_intrinsic(intrinsic!(match_const_pat), &mut [srcv, patv])?;
                self.bind_val(res.reflect(), resv)
            }
            SubstrIndex(dst, s, t) => self.binop(intrinsic!(substr_index), dst, s, t),
            SubstrLastIndex(dst, s, t) => self.binop(intrinsic!(substr_last_index), dst, s, t),
            LenStr(dst, x) => self.unop(intrinsic!(str_len), dst, x),
            Sub(res, pat, s, in_s) => {
                let rt = self.runtime_val();
                let patv = self.get_val(pat.reflect())?;
                let sv = self.get_val(s.reflect())?;
                let in_sv = self.get_val(in_s.reflect())?;
                let resv =
                    self.call_intrinsic(intrinsic!(subst_first), &mut [rt, patv, sv, in_sv])?;
                self.bind_val(res.reflect(), resv)
            }
            GSub(res, pat, s, in_s) => {
                let rt = self.runtime_val();
                let patv = self.get_val(pat.reflect())?;
                let sv = self.get_val(s.reflect())?;
                let in_sv = self.get_val(in_s.reflect())?;
                let resv =
                    self.call_intrinsic(intrinsic!(subst_all), &mut [rt, patv, sv, in_sv])?;
                self.bind_val(res.reflect(), resv)
            }
            GenSubDynamic(res, pat, s, how, in_s) => {
                let rt = self.runtime_val();
                let patv = self.get_val(pat.reflect())?;
                let sv = self.get_val(s.reflect())?;
                let howv = self.get_val(how.reflect())?;
                let in_sv = self.get_val(in_s.reflect())?;
                let resv =
                    self.call_intrinsic(intrinsic!(gen_subst), &mut [rt, patv, sv, howv, in_sv])?;
                self.bind_val(res.reflect(), resv)
            }
            EscapeCSV(dst, s) => self.unop(intrinsic!(escape_csv), dst, s),
            EscapeTSV(dst, s) => self.unop(intrinsic!(escape_tsv), dst, s),
            Substr(res, base, l, r) => {
                let basev = self.get_val(base.reflect())?;
                let lv = self.get_val(l.reflect())?;
                let rv = self.get_val(r.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(substr), &mut [basev, lv, rv])?;
                self.bind_val(res.reflect(), resv)
            }
            CharAt(res, text,index) => {
                let text = self.get_val(text.reflect())?;
                let index = self.get_val(index.reflect())?;
                //panic!("invalid index for chat_at: {}, should start with 1", index)
                let resv = self.call_intrinsic(intrinsic!(char_at), &mut [text, index])?;
                self.bind_val(res.reflect(), resv)
            }
            LastPart(res, s,sep) => {
                let s = self.get_val(s.reflect())?;
                let sep = self.get_val(sep.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(last_part), &mut [s, sep])?;
                self.bind_val(res.reflect(), resv)
            }
            LTInt(res, l, r) => self.binop(cmp(Cmp::Lt, false), res, l, r),
            GTInt(res, l, r) => self.binop(cmp(Cmp::Gt, false), res, l, r),
            LTEInt(res, l, r) => self.binop(cmp(Cmp::Lte, false), res, l, r),
            GTEInt(res, l, r) => self.binop(cmp(Cmp::Gte, false), res, l, r),
            EQInt(res, l, r) => self.binop(cmp(Cmp::Eq, false), res, l, r),
            LTFloat(res, l, r) => self.binop(cmp(Cmp::Lt, true), res, l, r),
            GTFloat(res, l, r) => self.binop(cmp(Cmp::Gt, true), res, l, r),
            LTEFloat(res, l, r) => self.binop(cmp(Cmp::Lte, true), res, l, r),
            GTEFloat(res, l, r) => self.binop(cmp(Cmp::Gte, true), res, l, r),
            EQFloat(res, l, r) => self.binop(cmp(Cmp::Eq, true), res, l, r),
            LTStr(res, l, r) => self.binop(intrinsic!(str_lt), res, l, r),
            GTStr(res, l, r) => self.binop(intrinsic!(str_gt), res, l, r),
            LTEStr(res, l, r) => self.binop(intrinsic!(str_lte), res, l, r),
            GTEStr(res, l, r) => self.binop(intrinsic!(str_gte), res, l, r),
            EQStr(res, l, r) => self.binop(intrinsic!(str_eq), res, l, r),
            SetColumn(dst, src) => {
                let rt = self.runtime_val();
                let srcv = self.get_val(src.reflect())?;
                let dstv = self.get_val(dst.reflect())?;
                self.call_void(external!(set_col), &mut [rt, dstv, srcv])?;
                Ok(())
            }
            GetColumn(dst, src) => {
                let rt = self.runtime_val();
                let srcv = self.get_val(src.reflect())?;
                let dstv = self.call_intrinsic(intrinsic!(get_col), &mut [rt, srcv])?;
                self.bind_val(dst.reflect(), dstv)
            }
            JoinCSV(dst, start, end) => {
                let rt = self.runtime_val();
                let startv = self.get_val(start.reflect())?;
                let endv = self.get_val(end.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(join_csv), &mut [rt, startv, endv])?;
                self.bind_val(dst.reflect(), resv)
            }
            JoinTSV(dst, start, end) => {
                let rt = self.runtime_val();
                let startv = self.get_val(start.reflect())?;
                let endv = self.get_val(end.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(join_tsv), &mut [rt, startv, endv])?;
                self.bind_val(dst.reflect(), resv)
            }
            Uuid(dst, version) => {
                let version = self.get_val(version.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(uuid), &mut [version])?;
                self.bind_val(dst.reflect(),resv)
            }
            SnowFlake(dst, machine_id) => {
                let machine_id = self.get_val(machine_id.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(snowflake), &mut [machine_id])?;
                self.bind_val(dst.reflect(),resv)
            }
            Ulid(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(ulid), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Tsid(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(tsid), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Whoami(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(whoami), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Version(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(version), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Os(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(os), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            OsFamily(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(os_family), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Arch(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(arch), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Pwd(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(pwd), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            UserHome(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(user_home), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            LocalIp(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(local_ip), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Systime(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(systime), &mut [rt])?;
                self.bind_val(dst.reflect(),resv)
            }
            Encode(dst,format, text) => {
                let format = self.get_val(format.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(encode), &mut [format, text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Decode(dst,format, text) => {
                let format = self.get_val(format.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(decode), &mut [format, text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Escape(dst,format, text) => {
                let format = self.get_val(format.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(escape), &mut [format, text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Digest(dst,algorithm, text) => {
                let algorithm = self.get_val(algorithm.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(digest), &mut [algorithm, text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Hmac(dst,algorithm,key, text) => {
                let algorithm = self.get_val(algorithm.reflect())?;
                let key = self.get_val(key.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(hmac), &mut [algorithm, key, text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Jwt(dst,algorithm,key, payload) => {
                let algorithm = self.get_val(algorithm.reflect())?;
                let key = self.get_val(key.reflect())?;
                let payload = self.get_val(payload.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(jwt), &mut [algorithm, key, payload])?;
                self.bind_val(dst.reflect(),resv)
            }
            Dejwt(dst,key, token) => {
                let key = self.get_val(key.reflect())?;
                let token = self.get_val(token.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(dejwt), &mut [key, token])?;
                self.bind_val(dst.reflect(),resv)
            }
            Encrypt(dst,mode,plain_text,key) => {
                let mode = self.get_val(mode.reflect())?;
                let plain_text = self.get_val(plain_text.reflect())?;
                let key = self.get_val(key.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(encrypt), &mut [mode,plain_text, key])?;
                self.bind_val(dst.reflect(),resv)
            }
            Decrypt(dst,mode,encrypted_text,key) => {
                let mode = self.get_val(mode.reflect())?;
                let encrypted_text = self.get_val(encrypted_text.reflect())?;
                let key = self.get_val(key.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(decrypt), &mut [mode,encrypted_text, key])?;
                self.bind_val(dst.reflect(),resv)
            }
            Strftime(dst,format, timestamp) => {
                let format = self.get_val(format.reflect())?;
                let timestamp = self.get_val(timestamp.reflect())?;
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(strftime), &mut [rt, format, timestamp])?;
                self.bind_val(dst.reflect(),resv)
            }
            Mktime(dst,date_time_text,timezone) => {
                let date_time_text = self.get_val(date_time_text.reflect())?;
                let timezone = self.get_val(timezone.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(mktime), &mut [date_time_text, timezone])?;
                self.bind_val(dst.reflect(),resv)
            },
            Duration(dst,expr) => self.unop(intrinsic!(duration), dst, expr),
            MkBool(dst,text) => self.unop(intrinsic!(mkbool), dst, text),
            MkPassword(dst,len) => self.unop(intrinsic!(mkpass), dst, len),
            Fend(dst,src) => self.unop(intrinsic!(fend), dst, src),
            Url(dst,src) => self.unop(intrinsic!(url), dst, src),
            Record(dst,src) => self.unop(intrinsic!(record), dst, src),
            Message(dst,src) => self.unop(intrinsic!(message), dst, src),
            Pairs(dst,src, pair_sep,kv_sep) => {
                let src = self.get_val(src.reflect())?;
                let pair_sep = self.get_val(pair_sep.reflect())?;
                let kv_sep = self.get_val(kv_sep.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(pairs), &mut [src, pair_sep, kv_sep])?;
                self.bind_val(dst.reflect(), resv)
            }
            SemVer(dst,src) => self.unop(intrinsic!(semver), dst, src),
            Path(dst,src) => self.unop(intrinsic!(path), dst, src),
            DataUrl(dst,src) => self.unop(intrinsic!(data_url), dst, src),
            DateTime(dst,timestamp) => self.unop(intrinsic!(datetime), dst, timestamp),
            TypeOfArray(dst) => {
                let resv = self.call_intrinsic(intrinsic!(type_of_array), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            TypeOfNumber(dst) => {
                let resv = self.call_intrinsic(intrinsic!(type_of_number), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            TypeOfString(dst) => {
                let resv = self.call_intrinsic(intrinsic!(type_of_string), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            TypeOfUnassigned(dst) => {
                let resv = self.call_intrinsic(intrinsic!(type_of_unassigned), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsArrayTrue(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_array_true), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsArrayFalse(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_array_false), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsIntTrue(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_int_true), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsIntFalse(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_int_false), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsStrInt(dst,text) => {
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(is_str_int), &mut [text])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsNumTrue(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_num_true), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsNumFalse(dst) => {
                let resv = self.call_intrinsic(intrinsic!(is_num_false), &mut [])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsStrNum(dst,text) => {
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(is_str_num), &mut [text])?;
                self.bind_val(dst.reflect(), resv)
            }
            IsFormat(dst,format, text) => {
                let format = self.get_val(format.reflect())?;
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(is_format), &mut [format, text])?;
                self.bind_val(dst.reflect(), resv)
            }
            Shlex(dst,text) => self.unop(intrinsic!(shlex), dst, text),
            Tuple(dst,text) => self.unop(intrinsic!(tuple), dst, text),
            Flags(dst,text) => self.unop(intrinsic!(flags), dst, text),
            ParseArray(dst,text) => self.unop(intrinsic!(parse_array), dst, text),
            Hex2Rgb(dst,text) => self.unop(intrinsic!(hex2rgb), dst, text),
            Rgb2Hex(dst,red, green, blue) => {
                let red = self.get_val(red.reflect())?;
                let green = self.get_val(green.reflect())?;
                let blue = self.get_val(blue.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(rgb2hex), &mut [red, green, blue])?;
                self.bind_val(dst.reflect(), resv)
            }
            Variant(dst,text) => self.unop(intrinsic!(variant), dst, text),
            Func(dst,text) => self.unop(intrinsic!(func), dst, text),
            SqliteQuery(dst,db_path,sql) => {
                let db_path = self.get_val(db_path.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(sqlite_query), &mut [db_path, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            SqliteExecute(dst,db_path,sql) => {
                let db_path = self.get_val(db_path.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(sqlite_execute), &mut [db_path, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            LibsqlQuery(dst,db_path,sql) => {
                let db_path = self.get_val(db_path.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(libsql_query), &mut [db_path, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            LibsqlExecute(dst,db_path,sql) => {
                let db_path = self.get_val(db_path.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(libsql_execute), &mut [db_path, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            MysqlQuery(dst,db_url,sql) => {
                let db_url = self.get_val(db_url.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(mysql_query), &mut [db_url, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            MysqlExecute(dst,db_url,sql) => {
                let db_url = self.get_val(db_url.reflect())?;
                let sql = self.get_val(sql.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(mysql_execute), &mut [db_url, sql])?;
                self.bind_val(dst.reflect(), resv)
            }
            FromJson(dst,src) => self.unop(intrinsic!(from_json), dst, src),
            MapIntIntToJson(dst,arr) => self.unop(intrinsic!(map_int_int_to_json), dst, arr),
            MapIntFloatToJson(dst,arr) => self.unop(intrinsic!(map_int_float_to_json), dst, arr),
            MapIntStrToJson(dst,arr) => self.unop(intrinsic!(map_int_str_to_json), dst, arr),
            MapStrIntToJson(dst,arr) => self.unop(intrinsic!(map_str_int_to_json), dst, arr),
            MapStrFloatToJson(dst,arr) => self.unop(intrinsic!(map_str_float_to_json), dst, arr),
            MapStrStrToJson(dst,arr) => self.unop(intrinsic!(map_str_str_to_json), dst, arr),
            StrToJson(dst,text) => self.unop(intrinsic!(str_to_json), dst, text),
            IntToJson(dst,num) => self.unop(intrinsic!(int_to_json), dst, num),
            FloatToJson(dst,num) => self.unop(intrinsic!(float_to_json), dst, num),
            NullToJson(dst) => {
                let resv = self.call_intrinsic(intrinsic!(null_to_json), &mut [])?;
                self.bind_val(dst.reflect(),resv)
            }
            JsonValue(dst, json_text, json_path) => {
                let json_text = self.get_val(json_text.reflect())?;
                let json_path = self.get_val(json_path.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(json_value), &mut [json_text, json_path])?;
                self.bind_val(dst.reflect(),resv)
            }
            JsonQuery(dst, json_text, json_path) => {
                let json_text = self.get_val(json_text.reflect())?;
                let json_path = self.get_val(json_path.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(json_query), &mut [json_text, json_path])?;
                self.bind_val(dst.reflect(),resv)
            }
            HtmlValue(dst, html_text, selector) => {
                let html_text = self.get_val(html_text.reflect())?;
                let selector = self.get_val(selector.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(html_value), &mut [html_text, selector])?;
                self.bind_val(dst.reflect(),resv)
            }
            HtmlQuery(dst, html_text, selector) => {
                let html_text = self.get_val(html_text.reflect())?;
                let selector = self.get_val(selector.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(html_query), &mut [html_text, selector])?;
                self.bind_val(dst.reflect(),resv)
            }
            XmlValue(dst, xml_text, xpath) => {
                let xml_text = self.get_val(xml_text.reflect())?;
                let xpath = self.get_val(xpath.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(xml_value), &mut [xml_text, xpath])?;
                self.bind_val(dst.reflect(),resv)
            }
            XmlQuery(dst, xml_text, xpath) => {
                let xml_text = self.get_val(xml_text.reflect())?;
                let xpath = self.get_val(xpath.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(xml_query), &mut [xml_text, xpath])?;
                self.bind_val(dst.reflect(),resv)
            }
            DumpMapIntInt(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_int_int), &mut [arr])?;
                Ok(())
            },
            DumpMapIntFloat(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_int_float), &mut [arr])?;
                Ok(())
            },
            DumpMapIntStr(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_int_str), &mut [arr])?;
                Ok(())
            },
            DumpMapStrInt(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_str_int), &mut [arr])?;
                Ok(())
            },
            DumpMapStrFloat(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_str_float), &mut [arr])?;
                Ok(())
            },
            DumpMapStrStr(arr) => {
                let arr = self.get_val(arr.reflect())?;
                self.call_void(external!(dump_map_str_str), &mut [arr])?;
                Ok(())
            },
            DumpStr(text) => {
                let text = self.get_val(text.reflect())?;
                self.call_void(external!(dump_str), &mut [text])?;
                Ok(())
            },
            DumpInt(num) => {
                let num = self.get_val(num.reflect())?;
                self.call_void(external!(dump_int), &mut [num])?;
                Ok(())
            },
            DumpFloat(num) => {
                let num = self.get_val(num.reflect())?;
                self.call_void(external!(dump_float), &mut [num])?;
                Ok(())
            },
            DumpNull() => {
                self.call_void(external!(dump_null), &mut [])?;
                Ok(())
            }
            MapIntIntAsort(dst, arr,target) => {
                let arr = self.get_val(arr.reflect())?;
                let target = self.get_val(target.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_int_asort), &mut [arr, target])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntFloatAsort(dst, arr,target) => {
                let arr = self.get_val(arr.reflect())?;
                let target = self.get_val(target.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_float_asort), &mut [arr, target])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntStrAsort(dst, arr,target) => {
                let arr = self.get_val(arr.reflect())?;
                let target = self.get_val(target.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_str_asort), &mut [arr, target])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntIntJoin(dst, arr,sep) => {
                let arr = self.get_val(arr.reflect())?;
                let sep = self.get_val(sep.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_int_join), &mut [arr, sep])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntFloatJoin(dst, arr,sep) => {
                let arr = self.get_val(arr.reflect())?;
                let sep = self.get_val(sep.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_float_join), &mut [arr, sep])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntStrJoin(dst, arr,sep) => {
                let arr = self.get_val(arr.reflect())?;
                let sep = self.get_val(sep.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(map_int_str_join), &mut [arr, sep])?;
                self.bind_val(dst.reflect(),resv)
            },
            MapIntIntMax(dst,arr) => self.unop(intrinsic!(map_int_int_max), dst, arr),
            MapIntFloatMax(dst,arr) => self.unop(intrinsic!(map_int_float_max), dst, arr),
            MapIntIntMin(dst,arr) => self.unop(intrinsic!(map_int_int_min), dst, arr),
            MapIntFloatMin(dst,arr) => self.unop(intrinsic!(map_int_float_min), dst, arr),
            MapIntIntSum(dst,arr) => self.unop(intrinsic!(map_int_int_sum), dst, arr),
            MapIntFloatSum(dst,arr) => self.unop(intrinsic!(map_int_float_sum), dst, arr),
            MapIntIntMean(dst,arr) => self.unop(intrinsic!(map_int_int_mean), dst, arr),
            MapIntFloatMean(dst,arr) => self.unop(intrinsic!(map_int_float_mean), dst, arr),
            FromCsv(dst,src) => self.unop(intrinsic!(from_csv), dst, src),
            MapIntIntToCsv(dst,arr) => self.unop(intrinsic!(map_int_int_to_csv), dst, arr),
            MapIntFloatToCsv(dst,arr) => self.unop(intrinsic!(map_int_float_to_csv), dst, arr),
            MapIntStrToCsv(dst,arr) => self.unop(intrinsic!(map_int_str_to_csv), dst, arr),
            HttpGet(dst, url,headers) => {
                let url = self.get_val(url.reflect())?;
                let headers = self.get_val(headers.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(http_get), &mut [url, headers])?;
                self.bind_val(dst.reflect(),resv)
            },
            HttpPost(dst, url,headers, body) => {
                let url = self.get_val(url.reflect())?;
                let headers = self.get_val(headers.reflect())?;
                let body = self.get_val(body.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(http_post), &mut [url, headers, body])?;
                self.bind_val(dst.reflect(),resv)
            },
            S3Get(dst,bucket, object_name) => {
                let bucket = self.get_val(bucket.reflect())?;
                let object_name = self.get_val(object_name.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(s3_get), &mut [bucket, object_name])?;
                self.bind_val(dst.reflect(),resv)
            }
            S3Put(dst,bucket, object_name, body) => {
                let bucket = self.get_val(bucket.reflect())?;
                let object_name = self.get_val(object_name.reflect())?;
                let body = self.get_val(body.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(s3_put), &mut [bucket, object_name, body])?;
                self.bind_val(dst.reflect(),resv)
            }
            Trim(dst,src, pat) => {
                let src = self.get_val(src.reflect())?;
                let pat = self.get_val(pat.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(trim), &mut [src, pat])?;
                self.bind_val(dst.reflect(),resv)
            }
            Strtonum(dst,text) => self.unop(intrinsic!(strtonum), dst, text),
            FormatBytes(dst,size) => self.unop(intrinsic!(format_bytes), dst, size),
            ToBytes(dst,text) => self.unop(intrinsic!(to_bytes), dst, text),
            StartsWith(dst,text, prefix) => {
                let text = self.get_val(text.reflect())?;
                let prefix = self.get_val(prefix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(starts_with), &mut [text, prefix])?;
                self.bind_val(dst.reflect(),resv)
            }
            EndsWith(dst,text, suffix) => {
                let text = self.get_val(text.reflect())?;
                let suffix = self.get_val(suffix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(ends_with), &mut [text, suffix])?;
                self.bind_val(dst.reflect(),resv)
            }
            TextContains(dst,text, child) => {
                let text = self.get_val(text.reflect())?;
                let child = self.get_val(child.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(text_contains), &mut [text, child])?;
                self.bind_val(dst.reflect(),resv)
            }
            Capitalize(dst,text) => self.unop(intrinsic!(capitalize), dst, text),
            UnCapitalize(dst,text) => self.unop(intrinsic!(uncapitalize), dst, text),
            CamelCase(dst,text) => self.unop(intrinsic!(camel_case), dst, text),
            KebabCase(dst,text) => self.unop(intrinsic!(kebab_case), dst, text),
            SnakeCase(dst,text) => self.unop(intrinsic!(snake_case), dst, text),
            TitleCase(dst,text) => self.unop(intrinsic!(title_case), dst, text),
            PadLeft(dst,text, len, pad) => {
                let text = self.get_val(text.reflect())?;
                let len = self.get_val(len.reflect())?;
                let pad = self.get_val(pad.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(pad_left), &mut [text, len, pad])?;
                self.bind_val(dst.reflect(),resv)
            }
            PadRight(dst,text, len, pad) => {
                let text = self.get_val(text.reflect())?;
                let len = self.get_val(len.reflect())?;
                let pad = self.get_val(pad.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(pad_right), &mut [text, len, pad])?;
                self.bind_val(dst.reflect(),resv)
            }
            PadBoth(dst,text, len, pad) => {
                let text = self.get_val(text.reflect())?;
                let len = self.get_val(len.reflect())?;
                let pad = self.get_val(pad.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(pad_both), &mut [text, len, pad])?;
                self.bind_val(dst.reflect(),resv)
            }
            StrCmp(dst,text1, text2) => {
                let text1 = self.get_val(text1.reflect())?;
                let text2 = self.get_val(text2.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(strcmp), &mut [text1, text2])?;
                self.bind_val(dst.reflect(),resv)
            }
            Mask(dst,text) => self.unop(intrinsic!(mask), dst, text),
            Repeat(dst,text,n) => {
                let text = self.get_val(text.reflect())?;
                let n = self.get_val(n.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(repeat), &mut [text, n])?;
                self.bind_val(dst.reflect(),resv)
            }
            DefaultIfEmpty(dst,text,default_value) => {
                let text = self.get_val(text.reflect())?;
                let default_value = self.get_val(default_value.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(default_if_empty), &mut [text, default_value])?;
                self.bind_val(dst.reflect(),resv)
            }
            AppendIfMissing(dst,text,suffix) => {
                let text = self.get_val(text.reflect())?;
                let suffix = self.get_val(suffix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(append_if_missing), &mut [text, suffix])?;
                self.bind_val(dst.reflect(),resv)
            }
            PrependIfMissing(dst,text,prefix) => {
                let text = self.get_val(text.reflect())?;
                let prefix = self.get_val(prefix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(prepend_if_missing), &mut [text, prefix])?;
                self.bind_val(dst.reflect(),resv)
            }
            RemoveIfBegin(dst,text,prefix) => {
                let text = self.get_val(text.reflect())?;
                let prefix = self.get_val(prefix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(remove_if_begin), &mut [text, prefix])?;
                self.bind_val(dst.reflect(),resv)
            }
            RemoveIfEnd(dst,text,suffix) => {
                let text = self.get_val(text.reflect())?;
                let suffix = self.get_val(suffix.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(remove_if_end), &mut [text, suffix])?;
                self.bind_val(dst.reflect(),resv)
            }
            Quote(dst,text) => {
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(quote), &mut [text])?;
                self.bind_val(dst.reflect(),resv)
            }
            DoubleQuote(dst,text) => {
                let text = self.get_val(text.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(double_quote), &mut [text])?;
                self.bind_val(dst.reflect(),resv)
            }
            Words(dst,text) => self.unop(intrinsic!(words), dst, text),
            Truncate(dst,src, len, place_holder) => {
                let src = self.get_val(src.reflect())?;
                let len = self.get_val(len.reflect())?;
                let place_holder = self.get_val(place_holder.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(truncate), &mut [src, len, place_holder])?;
                self.bind_val(dst.reflect(),resv)
            }
            Parse(dst,text, template) => {
                let text = self.get_val(text.reflect())?;
                let template = self.get_val(template.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(parse), &mut [text, template])?;
                self.bind_val(dst.reflect(),resv)
            }
            RegexParse(dst,text, template) => {
                let text = self.get_val(text.reflect())?;
                let template = self.get_val(template.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(rparse), &mut [text, template])?;
                self.bind_val(dst.reflect(),resv)
            }
            KvGet(dst,namespace, key) => {
                let namespace = self.get_val(namespace.reflect())?;
                let key = self.get_val(key.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(kv_get), &mut [namespace, key])?;
                self.bind_val(dst.reflect(),resv)
            }
            KvPut(namespace, key,value) => {
                let namespace = self.get_val(namespace.reflect())?;
                let key = self.get_val(key.reflect())?;
                let value = self.get_val(value.reflect())?;
                self.call_void(external!(kv_put), &mut [namespace, key, value])?;
                Ok(())
            }
            KvDelete(namespace, key) => {
                let namespace = self.get_val(namespace.reflect())?;
                let key = self.get_val(key.reflect())?;
                self.call_void(external!(kv_delete), &mut [namespace, key])?;
                Ok(())
            }
            KvClear(namespace) => {
                let namespace = self.get_val(namespace.reflect())?;
                self.call_void(external!(kv_clear), &mut [namespace])?;
                Ok(())
            }
            ReadAll(dst,path) => {
                let path = self.get_val(path.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(read_all), &mut [path])?;
                self.bind_val(dst.reflect(),resv)
            }
            WriteAll(path, content) => {
                let path = self.get_val(path.reflect())?;
                let content = self.get_val(content.reflect())?;
                self.call_void(external!(write_all), &mut [path, content])?;
                Ok(())
            }
            LogDebug(message) => {
                let message = self.get_val(message.reflect())?;
                let rt = self.runtime_val();
                self.call_void(external!(log_debug), &mut [rt, message])?;
                Ok(())
            }
            LogInfo(message) => {
                let message = self.get_val(message.reflect())?;
                let rt = self.runtime_val();
                self.call_void(external!(log_info), &mut [rt, message])?;
                Ok(())
            }
            LogWarn(message) => {
                let message = self.get_val(message.reflect())?;
                let rt = self.runtime_val();
                self.call_void(external!(log_warn), &mut [rt, message])?;
                Ok(())
            }
            LogError(message) => {
                let message = self.get_val(message.reflect())?;
                let rt = self.runtime_val();
                self.call_void(external!(log_error), &mut [rt, message])?;
                Ok(())
            }
            Publish(namespace, body) => {
                let namespace = self.get_val(namespace.reflect())?;
                let body = self.get_val(body.reflect())?;
                self.call_void(external!(publish), &mut [namespace, body])?;
                Ok(())
            }
            BloomFilterInsert(item, group) => {
                let item = self.get_val(item.reflect())?;
                let group = self.get_val(group.reflect())?;
                self.call_void(external!(bf_insert), &mut [item, group])?;
                Ok(())
            }
            BloomFilterContains(dst, item, group) => {
                let item = self.get_val(item.reflect())?;
                let group = self.get_val(group.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(bf_contains), &mut [item, group])?;
                self.bind_val(dst.reflect(),resv)
            }
            BloomFilterContainsWithInsert(dst, item, group) => {
                let item = self.get_val(item.reflect())?;
                let group = self.get_val(group.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(bf_icontains), &mut [item, group])?;
                self.bind_val(dst.reflect(),resv)
            }
            Fake(dst, data, locale) => {
                let data = self.get_val(data.reflect())?;
                let locale = self.get_val(locale.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(fake), &mut [data, locale])?;
                self.bind_val(dst.reflect(),resv)
            }
            Min(dst,first, second,third) => {
               let first = self.get_val(first.reflect())?;
               let second = self.get_val(second.reflect())?;
               let third = self.get_val(third.reflect())?;
               let resv = self.call_intrinsic(intrinsic!(min), &mut [first, second,third])?;
               self.bind_val(dst.reflect(),resv)
            }
            Max(dst,first, second,third) => {
               let first = self.get_val(first.reflect())?;
               let second = self.get_val(second.reflect())?;
               let third = self.get_val(third.reflect())?;
               let resv = self.call_intrinsic(intrinsic!(max), &mut [first, second, third])?;
               self.bind_val(dst.reflect(),resv)
            }
            Seq(dst,start, step,end) => {
                let start = self.get_val(start.reflect())?;
                let step = self.get_val(step.reflect())?;
                let end = self.get_val(end.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(seq), &mut [start, step, end])?;
                self.bind_val(dst.reflect(),resv)
            }
            Uniq(dst,src, param) => {
                let src = self.get_val(src.reflect())?;
                let param = self.get_val(param.reflect())?;
                let resv = self.call_intrinsic(intrinsic!(uniq), &mut [src, param])?;
                self.bind_val(dst.reflect(),resv)
            }
            JoinColumns(dst, start, end, sep) => {
                let rt = self.runtime_val();
                let startv = self.get_val(start.reflect())?;
                let endv = self.get_val(end.reflect())?;
                let sepv = self.get_val(sep.reflect())?;
                let resv =
                    self.call_intrinsic(intrinsic!(join_cols), &mut [rt, startv, endv, sepv])?;
                self.bind_val(dst.reflect(), resv)
            }
            SplitInt(flds, to_split, arr, pat) => {
                let rt = self.runtime_val();
                let tsv = self.get_val(to_split.reflect())?;
                let arrv = self.get_val(arr.reflect())?;
                let patv = self.get_val(pat.reflect())?;
                let fldsv =
                    self.call_intrinsic(intrinsic!(split_int), &mut [rt, tsv, arrv, patv])?;
                self.bind_val(flds.reflect(), fldsv)
            }
            SplitStr(flds, to_split, arr, pat) => {
                let rt = self.runtime_val();
                let tsv = self.get_val(to_split.reflect())?;
                let arrv = self.get_val(arr.reflect())?;
                let patv = self.get_val(pat.reflect())?;
                let fldsv =
                    self.call_intrinsic(intrinsic!(split_str), &mut [rt, tsv, arrv, patv])?;
                self.bind_val(flds.reflect(), fldsv)
            }
            Printf { output, fmt, args } => self.printf(output, fmt, &args[..]),
            Sprintf { dst, fmt, args } => self.sprintf(dst, fmt, &args[..]),
            PrintAll { output, args } => self.print_all(output, &args[..]),
            Close(file) => {
                let rt = self.runtime_val();
                let filev = self.get_val(file.reflect())?;
                self.call_void(external!(close_file), &mut [rt, filev])?;
                Ok(())
            }
            RunCmd(dst, cmd) => self.unop(intrinsic!(run_system), dst, cmd),
            Exit(code) => {
                let rt = self.runtime_val();
                let codev = self.get_val(code.reflect())?;
                self.call_void(external!(exit), &mut [rt, codev])?;
                Ok(())
            }
            ReadErr(dst, file, is_file) => {
                let rt = self.runtime_val();
                let filev = self.get_val(file.reflect())?;
                let is_filev = self.const_int(*is_file as i64);
                let resv = self.call_intrinsic(intrinsic!(read_err), &mut [rt, filev, is_filev])?;
                self.bind_val(dst.reflect(), resv)
            }
            NextLine(dst, file, is_file) => {
                let rt = self.runtime_val();
                let filev = self.get_val(file.reflect())?;
                let is_filev = self.const_int(*is_file as i64);
                let resv =
                    self.call_intrinsic(intrinsic!(next_line), &mut [rt, filev, is_filev])?;
                self.bind_val(dst.reflect(), resv)
            }
            ReadErrStdin(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(read_err_stdin), &mut [rt])?;
                self.bind_val(dst.reflect(), resv)
            }
            NextLineStdin(dst) => {
                let rt = self.runtime_val();
                let resv = self.call_intrinsic(intrinsic!(next_line_stdin), &mut [rt])?;
                self.bind_val(dst.reflect(), resv)
            }
            NextLineStdinFused() => {
                let rt = self.runtime_val();
                self.call_void(external!(next_line_stdin_fused), &mut [rt])?;
                Ok(())
            }
            NextFile() => {
                let rt = self.runtime_val();
                self.call_void(external!(next_file), &mut [rt])?;
                Ok(())
            }
            UpdateUsedFields() => {
                let rt = self.runtime_val();
                self.call_void(external!(update_used_fields), &mut [rt])?;
                Ok(())
            }
            SetFI(key, val) => {
                // We could probably get away without an extra intrinsic here, but this way we can
                // avoid repeated refs and drops of the FI variable outside of the existing
                // framework for performing refs and drops.
                let rt = self.runtime_val();
                let keyv = self.get_val(key.reflect())?;
                let valv = self.get_val(val.reflect())?;
                self.call_void(external!(set_fi_entry), &mut [rt, keyv, valv])?;
                Ok(())
            }
            Lookup {
                map_ty,
                dst,
                map,
                key,
            } => self.lookup_map(
                (*map, *map_ty),
                (*key, map_ty.key()?),
                (*dst, map_ty.val()?),
            ),
            Contains {
                map_ty,
                dst,
                map,
                key,
            } => self.contains_map(
                (*map, *map_ty),
                (*key, map_ty.key()?),
                (*dst, compile::Ty::Int),
            ),
            Delete { map_ty, map, key } => self.delete_map((*map, *map_ty), (*key, map_ty.key()?)),
            Clear { map_ty, map } => self.clear_map((*map, *map_ty)),
            Len { map_ty, map, dst } => self.len_map((*map, *map_ty), (*dst, compile::Ty::Int)),
            Store {
                map_ty,
                map,
                key,
                val,
            } => self.store_map(
                (*map, *map_ty),
                (*key, map_ty.key()?),
                (*val, map_ty.val()?),
            ),
            IncInt {
                map_ty,
                map,
                key,
                dst,
                by,
            } => self.inc_map(
                (*map, *map_ty),
                (*key, map_ty.key()?),
                by.reflect(),
                (*dst, map_ty.val()?),
            ),
            IncFloat {
                map_ty,
                map,
                key,
                dst,
                by,
            } => self.inc_map(
                (*map, *map_ty),
                (*key, map_ty.key()?),
                by.reflect(),
                (*dst, map_ty.val()?),
            ),
            LoadVarStr(dst, var) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let res = self.call_intrinsic(intrinsic!(load_var_str), &mut [rt, varv])?;
                let dref = dst.reflect();
                self.bind_val(dref, res)
            }
            StoreVarStr(var, src) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let srcv = self.get_val(src.reflect())?;
                self.call_void(external!(store_var_str), &mut [rt, varv, srcv])?;
                Ok(())
            }
            LoadVarInt(dst, var) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let res = self.call_intrinsic(intrinsic!(load_var_int), &mut [rt, varv])?;
                let dref = dst.reflect();
                self.bind_val(dref, res)
            }
            StoreVarInt(var, src) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let srcv = self.get_val(src.reflect())?;
                self.call_void(external!(store_var_int), &mut [rt, varv, srcv])?;
                Ok(())
            }
            LoadVarIntMap(dst, var) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let res = self.call_intrinsic(intrinsic!(load_var_intmap), &mut [rt, varv])?;
                let dref = dst.reflect();
                self.bind_val(dref, res)
            }
            StoreVarIntMap(var, src) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let srcv = self.get_val(src.reflect())?;
                self.call_void(external!(store_var_intmap), &mut [rt, varv, srcv])
            }
            LoadVarStrMap(dst, var) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let res = self.call_intrinsic(intrinsic!(load_var_strmap), &mut [rt, varv])?;
                let dref = dst.reflect();
                self.bind_val(dref, res)
            }
            StoreVarStrMap(var, src) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let srcv = self.get_val(src.reflect())?;
                self.call_void(external!(store_var_strmap), &mut [rt, varv, srcv])?;
                Ok(())
            }
            LoadVarStrStrMap(dst, var) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let res = self.call_intrinsic(intrinsic!(load_var_strstrmap), &mut [rt, varv])?;
                let dref = dst.reflect();
                self.bind_val(dref, res)
            }
            StoreVarStrStrMap(var, src) => {
                let rt = self.runtime_val();
                let varv = self.const_int(*var as i64);
                let srcv = self.get_val(src.reflect())?;
                self.call_void(external!(store_var_strstrmap), &mut [rt, varv, srcv])?;
                Ok(())
            }
            LoadSlot { ty, dst, slot } => self.load_slot((*dst, *ty), *slot),
            StoreSlot { ty, src, slot } => self.store_slot((*src, *ty), *slot),
            Mov(ty, dst, src) => self.mov(*ty, *dst, *src),
            IterBegin { map_ty, map, dst } => {
                self.iter_begin((*dst, map_ty.key_iter()?), (*map, *map_ty))
            }
            IterHasNext { iter_ty, dst, iter } => {
                self.iter_hasnext((*dst, compile::Ty::Int), (*iter, *iter_ty))
            }
            IterGetNext { iter_ty, dst, iter } => {
                self.iter_getnext((*dst, iter_ty.iter()?), (*iter, *iter_ty))
            }
            Push(_, _) | Pop(_, _) => err!("unexpected explicit push/pop in llvm"),
            AllocMap(_, _) => {
                err!("unexpected AllocMap (allocs are handled differently in LLVM)")
            }
            Ret | Jmp(_) | JmpIf(_, _) | Call(_) => {
                err!("unexpected bytecode-level control flow")
            }
        }
    }
}

fn map_key_valid(map: compile::Ty, key: compile::Ty) -> Result<()> {
    if map.key()? != key {
        return err!("map key type does not match: {:?} vs {:?}", map, key);
    }
    Ok(())
}

fn map_valid(map: compile::Ty, key: compile::Ty, val: compile::Ty) -> Result<()> {
    map_key_valid(map, key)?;
    if map.val()? != val {
        return err!("map value type does not match: {:?} vs {:?}", map, val);
    }
    Ok(())
}
