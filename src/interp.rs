use crate::builtins::Variable;
use crate::bytecode::{Get, Instr, Label, Reg};
use crate::common::{NumTy, Result, Stage};
use crate::compile::{self, Ty};
use crate::pushdown::FieldSet;
use crate::runtime::{self, Float, Int, Line, LineReader, Str, UniqueStr};

use crossbeam::scope;
use crossbeam_channel::bounded;
use hashbrown::HashMap;
use rand::{self, rngs::StdRng, Rng, SeedableRng};
use regex::bytes::Regex;

use std::mem;
use std::time::SystemTime;
use crate::builtins;

type ClassicReader = runtime::splitter::regex::RegexSplitter<Box<dyn std::io::Read>>;

#[derive(Default)]
pub(crate) struct Storage<T> {
    pub(crate) regs: Vec<T>,
    pub(crate) stack: Vec<T>,
}

/// Core represents a subset of runtime structures that are relevant to both the bytecode
/// interpreter and the compiled runtimes.
pub(crate) struct Core<'a> {
    pub vars: runtime::Variables<'a>,
    pub regexes: runtime::RegexCache,
    pub write_files: runtime::FileWrite,
    pub rng: StdRng,
    pub current_seed: u64,
    pub slots: Slots,
}

impl<'a> Drop for Core<'a> {
    fn drop(&mut self) {
        if let Err(e) = self.write_files.shutdown() {
            eprintln_ignore!("{}", e);
        }
    }
}

/// Slots are used for transmitting data across different "stages" of a parallel computation. In
/// order to send data to and from worker threads, we load and store them into dynamically-sized
/// "slots". These aren't normal registers, because slots store `Send` variants of the frawk
/// runtime types; making the value safe for sending between threads may involve performing a
/// deep copy.
#[derive(Default, Clone)]
pub(crate) struct Slots {
    pub int: Vec<Int>,
    pub float: Vec<Float>,
    pub strs: Vec<UniqueStr<'static>>,
    pub intint: Vec<HashMap<Int, Int>>,
    pub intfloat: Vec<HashMap<Int, Float>>,
    pub intstr: Vec<HashMap<Int, UniqueStr<'static>>>,
    pub strint: Vec<HashMap<UniqueStr<'static>, Int>>,
    pub strfloat: Vec<HashMap<UniqueStr<'static>, Float>>,
    pub strstr: Vec<HashMap<UniqueStr<'static>, UniqueStr<'static>>>,
}

/// A Simple helper trait for implement aggregations for slot values and variables.
trait Agg {
    fn agg(self, other: Self) -> Self;
}

impl Agg for Int {
    fn agg(self, other: Int) -> Int {
        self + other
    }
}

impl Agg for Float {
    fn agg(self, other: Float) -> Float {
        self + other
    }
}

impl<'a> Agg for UniqueStr<'a> {
    fn agg(self, other: UniqueStr<'a>) -> UniqueStr<'a> {
        // Strings are not aggregated explicitly.
        if other.is_empty() {
            self
        } else {
            other
        }
    }
}

impl<K: std::hash::Hash + Eq, V: Agg + Default> Agg for HashMap<K, V> {
    fn agg(mut self, other: HashMap<K, V>) -> HashMap<K, V> {
        for (k, v) in other {
            let entry = self.entry(k).or_default();
            let v2 = mem::take(entry);
            *entry = v2.agg(v);
        }
        self
    }
}

/// StageResult is a Send subset of Core that can be extracted for inter-stage aggregation in a
/// parallel script.
pub(crate) struct StageResult {
    slots: Slots,
    // TODO: put more variables in here? Most builtin variables are just going to be propagated
    // from the initial thread.
    nr: Int,
    rc: i32,
}

impl Slots {
    fn combine(&mut self, mut other: Slots) {
        macro_rules! for_each_slot_pair {
            ($s1:ident, $s2:ident, $body:expr) => {
                for_each_slot_pair!(
                    $s1, $s2, $body, int, float, strs, intint, intfloat, intstr, strint, strfloat,
                    strstr
                );
            };
            ($s1:ident, $s2:ident, $body:expr, $($fld:tt),*) => {$({
                let $s1 = &mut self.$fld;
                let $s2 = &mut other.$fld;
                $body
            });*};
        }

        for_each_slot_pair!(a, b, {
            a.resize_with(std::cmp::max(a.len(), b.len()), Default::default);
            for (a_elt, b_elt_v) in a.iter_mut().zip(b.drain(..)) {
                let a_elt_v = mem::take(a_elt);
                *a_elt = a_elt_v.agg(b_elt_v);
            }
        });
    }
}

pub fn set_slot<T: Default>(vec: &mut Vec<T>, slot: usize, v: T) {
    if slot < vec.len() {
        vec[slot] = v;
        return;
    }
    vec.resize_with(slot, Default::default);
    vec.push(v)
}

pub fn combine_slot<T: Default>(vec: &mut Vec<T>, slot: usize, f: impl FnOnce(T) -> T) {
    if slot < vec.len() {
        let res = f(std::mem::take(&mut vec[slot]));
        vec[slot] = res;
        return;
    }
    vec.resize_with(slot, Default::default);
    let res = f(Default::default());
    vec.push(res)
}

impl<'a> Core<'a> {
    pub fn shuttle(&self, pid: Int) -> impl FnOnce() -> Core<'a> + Send {
        use crate::builtins::Variables;
        let seed: u64 = rand::thread_rng().gen();
        let fw = self.write_files.clone();
        let fs: UniqueStr<'a> = self.vars.fs.clone().into();
        let ofs: UniqueStr<'a> = self.vars.ofs.clone().into();
        let rs: UniqueStr<'a> = self.vars.rs.clone().into();
        let ors: UniqueStr<'a> = self.vars.ors.clone().into();
        let filename: UniqueStr<'a> = self.vars.filename.clone().into();
        let argv = self.vars.argv.shuttle();
        let fi = self.vars.fi.shuttle();
        let environ = self.vars.environ.shuttle();
        let procinfo = self.vars.procinfo.shuttle();
        let slots = self.slots.clone();
        move || {
            let vars = Variables {
                fs: fs.into_str(),
                ofs: ofs.into_str(),
                ors: ors.into_str(),
                rs: rs.into_str(),
                filename: filename.into_str(),
                pid,
                nf: 0,
                nr: 0,
                fnr: 0,
                rstart: 0,
                rlength: 0,
                argc: 0,
                argv: argv.into(),
                fi: fi.into(),
                environ: environ.into(),
                procinfo: procinfo.into(),
            };
            Core {
                vars,
                regexes: Default::default(),
                write_files: fw,
                rng: rand::rngs::StdRng::seed_from_u64(seed),
                current_seed: seed,
                slots,
            }
        }
    }
    pub fn new(ff: impl runtime::writers::FileFactory) -> Core<'a> {
        let seed: u64 = rand::thread_rng().gen();
        Core {
            vars: Default::default(),
            regexes: Default::default(),
            write_files: runtime::FileWrite::new(ff),
            rng: rand::rngs::StdRng::seed_from_u64(seed),
            current_seed: seed,
            slots: Default::default(),
        }
    }

    pub fn extract_result(&mut self, rc: i32) -> StageResult {
        StageResult {
            slots: mem::take(&mut self.slots),
            nr: self.vars.nr,
            rc,
        }
    }

    pub fn combine(&mut self, StageResult { slots, nr, rc: _ }: StageResult) {
        self.slots.combine(slots);
        self.vars.nr = self.vars.nr.agg(nr);
    }

    pub fn reseed(&mut self, seed: u64) -> u64 /* old seed */ {
        self.rng = StdRng::seed_from_u64(seed);
        let old_seed = self.current_seed;
        self.current_seed = seed;
        old_seed
    }

    pub fn reseed_random(&mut self) -> u64 /* old seed */ {
        self.reseed(rand::thread_rng().gen::<u64>())
    }

    pub fn match_regex(&mut self, s: &Str<'a>, pat: &Str<'a>) -> Result<Int> {
        self.regexes.regex_match_loc(&mut self.vars, pat, s)
    }

    pub fn match_const_regex(&mut self, s: &Str<'a>, pat: &Regex) -> Result<Int> {
        runtime::RegexCache::regex_const_match_loc(&mut self.vars, pat, s)
    }

    pub fn is_match_regex(&mut self, s: &Str<'a>, pat: &Str<'a>) -> Result<bool> {
        self.regexes.is_regex_match(pat, s)
    }

    pub fn load_int(&mut self, slot: usize) -> Int {
        self.slots.int[slot]
    }
    pub fn load_float(&mut self, slot: usize) -> Float {
        self.slots.float[slot]
    }
    pub fn load_str(&mut self, slot: usize) -> Str<'a> {
        mem::take(&mut self.slots.strs[slot]).into_str().upcast()
    }
    pub fn load_intint(&mut self, slot: usize) -> runtime::IntMap<Int> {
        mem::take(&mut self.slots.intint[slot]).into()
    }
    pub fn load_intfloat(&mut self, slot: usize) -> runtime::IntMap<Float> {
        mem::take(&mut self.slots.intfloat[slot]).into()
    }
    pub fn load_intstr(&mut self, slot: usize) -> runtime::IntMap<Str<'a>> {
        mem::take(&mut self.slots.intstr[slot])
            .into_iter()
            .map(|(k, v)| (k, v.into_str().upcast()))
            .collect()
    }
    pub fn load_strint(&mut self, slot: usize) -> runtime::StrMap<'a, Int> {
        mem::take(&mut self.slots.strint[slot])
            .into_iter()
            .map(|(k, v)| (k.into_str().upcast(), v))
            .collect()
    }
    pub fn load_strfloat(&mut self, slot: usize) -> runtime::StrMap<'a, Float> {
        mem::take(&mut self.slots.strfloat[slot])
            .into_iter()
            .map(|(k, v)| (k.into_str().upcast(), v))
            .collect()
    }
    pub fn load_strstr(&mut self, slot: usize) -> runtime::StrMap<'a, Str<'a>> {
        mem::take(&mut self.slots.strstr[slot])
            .into_iter()
            .map(|(k, v)| (k.into_str().upcast(), v.into_str().upcast()))
            .collect()
    }

    pub fn store_int(&mut self, slot: usize, i: Int) {
        set_slot(&mut self.slots.int, slot, i)
    }
    pub fn store_float(&mut self, slot: usize, f: Float) {
        set_slot(&mut self.slots.float, slot, f)
    }
    pub fn store_str(&mut self, slot: usize, s: Str<'a>) {
        set_slot(&mut self.slots.strs, slot, s.unmoor().into())
    }
    pub fn store_intint(&mut self, slot: usize, s: runtime::IntMap<Int>) {
        set_slot(
            &mut self.slots.intint,
            slot,
            s.iter(|i| i.map(|(k, v)| (*k, *v)).collect()),
        )
    }
    pub fn store_intfloat(&mut self, slot: usize, s: runtime::IntMap<Float>) {
        set_slot(
            &mut self.slots.intfloat,
            slot,
            s.iter(|i| i.map(|(k, v)| (*k, *v)).collect()),
        )
    }
    pub fn store_intstr(&mut self, slot: usize, s: runtime::IntMap<Str<'a>>) {
        set_slot(
            &mut self.slots.intstr,
            slot,
            s.iter(|i| i.map(|(k, v)| (*k, v.clone().unmoor().into())).collect()),
        )
    }
    pub fn store_strint(&mut self, slot: usize, s: runtime::StrMap<'a, Int>) {
        set_slot(
            &mut self.slots.strint,
            slot,
            s.iter(|i| i.map(|(k, v)| (k.clone().unmoor().into(), *v)).collect()),
        )
    }
    pub fn store_strfloat(&mut self, slot: usize, s: runtime::StrMap<'a, Float>) {
        set_slot(
            &mut self.slots.strfloat,
            slot,
            s.iter(|i| i.map(|(k, v)| (k.clone().unmoor().into(), *v)).collect()),
        )
    }
    pub fn store_strstr(&mut self, slot: usize, s: runtime::StrMap<'a, Str<'a>>) {
        set_slot(
            &mut self.slots.strstr,
            slot,
            s.iter(|i| {
                i.map(|(k, v)| (k.clone().unmoor().into(), v.clone().unmoor().into()))
                    .collect()
            }),
        )
    }
}

macro_rules! map_regs {
    ($map_ty:expr, $map_reg:ident, $body:expr) => {{
        let _placeholder_k = 0u32;
        let _placeholder_v = 0u32;
        map_regs!($map_ty, $map_reg, _placeholder_k, _placeholder_v, $body)
    }};
    ($map_ty:expr, $map_reg:ident, $key_reg:ident, $val_reg:ident, $body:expr) => {{
        let _placeholder_iter = 0u32;
        map_regs!(
            $map_ty,
            $map_reg,
            $key_reg,
            $val_reg,
            _placeholder_iter,
            $body
        )
    }};
    ($map_ty:expr, $map_reg:ident, $key_reg:ident, $val_reg:ident, $iter_reg:ident, $body:expr) => {{
        let map_ty = $map_ty;
        match map_ty {
            Ty::MapIntInt => {
                let $map_reg: Reg<runtime::IntMap<Int>> = $map_reg.into();
                let $key_reg: Reg<Int> = $key_reg.into();
                let $val_reg: Reg<Int> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Int>> = $iter_reg.into();
                $body
            }
            Ty::MapIntFloat => {
                let $map_reg: Reg<runtime::IntMap<Float>> = $map_reg.into();
                let $key_reg: Reg<Int> = $key_reg.into();
                let $val_reg: Reg<Float> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Int>> = $iter_reg.into();
                $body
            }
            Ty::MapIntStr => {
                let $map_reg: Reg<runtime::IntMap<Str<'a>>> = $map_reg.into();
                let $key_reg: Reg<Int> = $key_reg.into();
                let $val_reg: Reg<Str<'a>> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Int>> = $iter_reg.into();
                $body
            }
            Ty::MapStrInt => {
                let $map_reg: Reg<runtime::StrMap<'a, Int>> = $map_reg.into();
                let $key_reg: Reg<Str<'a>> = $key_reg.into();
                let $val_reg: Reg<Int> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Str<'a>>> = $iter_reg.into();
                $body
            }
            Ty::MapStrFloat => {
                let $map_reg: Reg<runtime::StrMap<'a, Float>> = $map_reg.into();
                let $key_reg: Reg<Str<'a>> = $key_reg.into();
                let $val_reg: Reg<Float> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Str<'a>>> = $iter_reg.into();
                $body
            }
            Ty::MapStrStr => {
                let $map_reg: Reg<runtime::StrMap<'a, Str<'a>>> = $map_reg.into();
                let $key_reg: Reg<Str<'a>> = $key_reg.into();
                let $val_reg: Reg<Str<'a>> = $val_reg.into();
                let $iter_reg: Reg<runtime::Iter<Str<'a>>> = $iter_reg.into();
                $body
            }
            Ty::Null | Ty::Int | Ty::Float | Ty::Str | Ty::IterInt | Ty::IterStr => panic!(
                "attempting to perform map operations on non-map type: {:?}",
                map_ty
            ),
        }
    }};
}

pub(crate) struct Interp<'a, LR: LineReader = ClassicReader> {
    // index of `instrs` that contains "main"
    main_func: Stage<usize>,
    num_workers: usize,
    instrs: Vec<Vec<Instr<'a>>>,
    stack: Vec<(usize /*function*/, Label /*instr*/)>,

    line: LR::Line,
    read_files: runtime::FileRead<LR>,

    core: Core<'a>,

    // Core storage.
    // TODO: should these be smallvec<[T; 32]>? We never add registers, so could we allocate one
    // contiguous region ahead of time?
    pub(crate) floats: Storage<Float>,
    pub(crate) ints: Storage<Int>,
    pub(crate) strs: Storage<Str<'a>>,
    pub(crate) maps_int_float: Storage<runtime::IntMap<Float>>,
    pub(crate) maps_int_int: Storage<runtime::IntMap<Int>>,
    pub(crate) maps_int_str: Storage<runtime::IntMap<Str<'a>>>,

    pub(crate) maps_str_float: Storage<runtime::StrMap<'a, Float>>,
    pub(crate) maps_str_int: Storage<runtime::StrMap<'a, Int>>,
    pub(crate) maps_str_str: Storage<runtime::StrMap<'a, Str<'a>>>,

    pub(crate) iters_int: Storage<runtime::Iter<Int>>,
    pub(crate) iters_str: Storage<runtime::Iter<Str<'a>>>,
}

fn default_of<T: Default>(n: usize) -> Storage<T> {
    let mut regs = Vec::new();
    regs.resize_with(n, Default::default);
    Storage {
        regs,
        stack: Default::default(),
    }
}

impl<'a, LR: LineReader> Interp<'a, LR> {
    pub(crate) fn new(
        instrs: Vec<Vec<Instr<'a>>>,
        main_func: Stage<usize>,
        num_workers: usize,
        regs: impl Fn(compile::Ty) -> usize,
        stdin: LR,
        ff: impl runtime::writers::FileFactory,
        used_fields: &FieldSet,
        named_columns: Option<Vec<&[u8]>>,
    ) -> Self {
        use compile::Ty::*;
        Interp {
            main_func,
            num_workers,
            instrs,
            stack: Default::default(),
            floats: default_of(regs(Float)),
            ints: default_of(regs(Int)),
            strs: default_of(regs(Str)),
            core: Core::new(ff),

            line: Default::default(),
            read_files: runtime::FileRead::new(stdin, used_fields.clone(), named_columns),

            maps_int_float: default_of(regs(MapIntFloat)),
            maps_int_int: default_of(regs(MapIntInt)),
            maps_int_str: default_of(regs(MapIntStr)),

            maps_str_float: default_of(regs(MapStrFloat)),
            maps_str_int: default_of(regs(MapStrInt)),
            maps_str_str: default_of(regs(MapStrStr)),

            iters_int: default_of(regs(IterInt)),
            iters_str: default_of(regs(IterStr)),
        }
    }

    pub(crate) fn instrs(&self) -> &Vec<Vec<Instr<'a>>> {
        &self.instrs
    }

    fn format_arg(&self, (reg, ty): (NumTy, Ty)) -> Result<runtime::FormatArg<'a>> {
        Ok(match ty {
            Ty::Str => self.get(Reg::<Str<'a>>::from(reg)).clone().into(),
            Ty::Int => (*self.get(Reg::<Int>::from(reg))).into(),
            Ty::Float => (*self.get(Reg::<Float>::from(reg))).into(),
            Ty::Null => runtime::FormatArg::Null,
            _ => return err!("non-scalar (s)printf argument type {:?}", ty),
        })
    }

    fn reset_file_vars(&mut self) {
        self.core.vars.fnr = 0;
        self.core.vars.filename = self.read_files.stdin_filename().upcast();
    }

    pub(crate) fn run_parallel(&mut self) -> Result<i32> {
        if self.num_workers <= 1 {
            return self.run_serial();
        }
        let handles = self.read_files.try_resize(self.num_workers - 1);
        if handles.is_empty() {
            return self.run_serial();
        }
        let (begin, middle, end) = match self.main_func {
            Stage::Par {
                begin,
                main_loop,
                end,
            } => (begin, main_loop, end),
            Stage::Main(_) => {
                return err!("unexpected Main-only configuration for parallel execution");
            }
        };
        let main_loop = if let Some(main_loop) = middle {
            main_loop
        } else {
            return self.run_serial();
        };
        if let Some(off) = begin {
            let rc = self.run_at(off)?;
            if rc != 0 {
                return Ok(rc);
            }
        }
        if self.core.write_files.flush_stdout().is_err() {
            return Ok(1);
        }
        // For handling the worker portion, we want to transfer the current stdin progress to a
        // worker thread, but to withhold any progress on other files open for read. We'll swap
        // these back in when we execute the `end` block, if there is one.
        let mut old_read_files = mem::take(&mut self.read_files.inputs);
        fn wrap_error<T, S>(r: std::result::Result<Result<T>, S>) -> Result<T> {
            match r {
                Ok(Ok(t)) => Ok(t),
                Ok(Err(e)) => Err(e),
                Err(_) => err!("error in executing worker thread"),
            }
        }
        let scope_res = scope(|s| {
            let (sender, receiver) = bounded(handles.len());
            let float_size = self.floats.regs.len();
            let ints_size = self.ints.regs.len();
            let strs_size = self.strs.regs.len();
            let maps_int_int_size = self.maps_int_int.regs.len();
            let maps_int_float_size = self.maps_int_float.regs.len();
            let maps_int_str_size = self.maps_int_str.regs.len();
            let maps_str_int_size = self.maps_str_int.regs.len();
            let maps_str_float_size = self.maps_str_float.regs.len();
            let maps_str_str_size = self.maps_str_str.regs.len();
            let iters_int_size = self.iters_int.regs.len();
            let iters_str_size = self.iters_str.regs.len();
            for (i, handle) in handles.into_iter().enumerate() {
                let sender = sender.clone();
                let core_shuttle = self.core.shuttle(i as Int + 2);
                let instrs = self.instrs.clone();
                s.spawn(move |_| {
                    if let Some(read_files) = handle() {
                        let mut interp = Interp {
                            main_func: Stage::Main(main_loop),
                            num_workers: 1,
                            instrs,
                            stack: Default::default(),
                            core: core_shuttle(),
                            line: Default::default(),
                            read_files,

                            floats: default_of(float_size),
                            ints: default_of(ints_size),
                            strs: default_of(strs_size),
                            maps_int_int: default_of(maps_int_int_size),
                            maps_int_float: default_of(maps_int_float_size),
                            maps_int_str: default_of(maps_int_str_size),
                            maps_str_int: default_of(maps_str_int_size),
                            maps_str_float: default_of(maps_str_float_size),
                            maps_str_str: default_of(maps_str_str_size),
                            iters_int: default_of(iters_int_size),
                            iters_str: default_of(iters_str_size),
                        };
                        let res = interp.run_at(main_loop);

                        // Ignore errors, as it means another thread executed with an error and we are
                        // exiting anyway.
                        let _ = match res {
                            Err(e) => sender.send(Err(e)),
                            Ok(rc) => sender.send(Ok(interp.core.extract_result(rc))),
                        };
                    }
                });
            }
            mem::drop(sender);
            self.core.vars.pid = 1;
            let mut rc = self.run_at(main_loop)?;
            self.core.vars.pid = 0;
            while let Ok(res) = receiver.recv() {
                let res = res?;
                let sub_rc = res.rc;
                self.core.combine(res);
                if rc == 0 && sub_rc != 0 {
                    rc = sub_rc;
                }
            }
            Ok(rc)
        });
        let rc = wrap_error(scope_res)?;
        if rc != 0 {
            return Ok(rc);
        }
        if let Some(end) = end {
            mem::swap(&mut self.read_files.inputs, &mut old_read_files);
            Ok(self.run_at(end)?)
        } else {
            Ok(0)
        }
    }

    pub(crate) fn run_serial(&mut self) -> Result<i32> {
        let offs: smallvec::SmallVec<[usize; 3]> = self.main_func.iter().cloned().collect();
        for off in offs.into_iter() {
            let rc = self.run_at(off)?;
            if rc != 0 {
                return Ok(rc);
            }
        }
        Ok(0)
    }

    pub(crate) fn run(&mut self) -> Result<i32> {
        match self.main_func {
            Stage::Main(_) => self.run_serial(),
            Stage::Par { .. } => self.run_parallel(),
        }
    }

    #[allow(clippy::never_loop)]
    pub(crate) fn run_at(&mut self, mut cur_fn: usize) -> Result<i32> {
        use Instr::*;
        let mut scratch: Vec<runtime::FormatArg> = Vec::new();
        // We are only accessing one vector at a time here, but it's hard to convince the borrow
        // checker of this fact, so we access the vectors through raw pointers.
        let mut instrs = (&mut self.instrs[cur_fn]) as *mut Vec<Instr<'a>>;
        let mut cur = 0;

        'outer: loop {
            // This somewhat ersatz structure is to allow 'cur' to be reassigned
            // in most but not all branches in the big match below.
            cur = loop {
                debug_assert!(cur < unsafe { (*instrs).len() });
                use Variable::*;
                match unsafe { (*instrs).get_unchecked(cur) } {
                    StoreConstStr(sr, s) => {
                        let sr = *sr;
                        *self.get_mut(sr) = s.clone_str()
                    }
                    StoreConstInt(ir, i) => {
                        let ir = *ir;
                        *self.get_mut(ir) = *i
                    }
                    StoreConstFloat(fr, f) => {
                        let fr = *fr;
                        *self.get_mut(fr) = *f
                    }
                    IntToStr(sr, ir) => {
                        let s = runtime::convert::<_, Str>(*self.get(*ir));
                        let sr = *sr;
                        *self.get_mut(sr) = s;
                    }
                    FloatToStr(sr, fr) => {
                        let s = runtime::convert::<_, Str>(*self.get(*fr));
                        let sr = *sr;
                        *self.get_mut(sr) = s;
                    }
                    Uuid(dst, version) => {
                        let version = index(&self.strs, version);
                        let res = Str::from(runtime::math_util::uuid(version.as_str()));
                        *index_mut(&mut self.strs, dst) = res;
                    }
                    SnowFlake(dst, machine_id) => {
                        let machine_id: i64 = *self.get(*machine_id);
                        let res = runtime::math_util::snowflake(machine_id as u16);
                        let dst = *dst;
                        *self.get_mut(dst) = res
                    }
                    Ulid(dst) => {
                        let ulid = Str::from(runtime::math_util::ulid());
                        *index_mut(&mut self.strs, dst) = ulid;
                    }
                    Tsid(dst) => {
                        let tsid = Str::from(runtime::math_util::tsid());
                        *index_mut(&mut self.strs, dst) = tsid;
                    }
                    Whoami(dst) => {
                        let username = Str::from(whoami::username());
                        *index_mut(&mut self.strs, dst) = username;
                    }
                    Version(dst) => {
                        let zawk_version = Str::from(builtins::VERSION);
                        *index_mut(&mut self.strs, dst) = zawk_version;
                    }
                    Os(dst) => {
                        let os = Str::from(runtime::os_util::os());
                        *index_mut(&mut self.strs, dst) = os;
                    }
                    OsFamily(dst) => {
                        let os_family = Str::from(runtime::os_util::os_family());
                        *index_mut(&mut self.strs, dst) = os_family;
                    }
                    Arch(dst) => {
                        let arch = Str::from(runtime::os_util::arch());
                        *index_mut(&mut self.strs, dst) = arch;
                    }
                    Pwd(dst) => {
                        let pwd = Str::from(runtime::os_util::pwd());
                        *index_mut(&mut self.strs, dst) = pwd;
                    }
                    UserHome(dst) => {
                        let user_home = Str::from(runtime::os_util::user_home());
                        *index_mut(&mut self.strs, dst) = user_home;
                    }
                    LocalIp(dst) => {
                        let local_ip = Str::from(runtime::network::local_ip());
                        *index_mut(&mut self.strs, dst) = local_ip;
                    }
                    Systime(dst) => {
                        let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
                        let result: u64 = now.as_secs();
                        let ir = *dst;
                        *self.get_mut(ir) = result as Int;
                    }
                    Encode(dst, format, text) => {
                        let format = index(&self.strs, format);
                        let text = index(&self.strs, text);
                        let dt_text = runtime::encoding::encode(format.as_str(), text.as_str());
                        *index_mut(&mut self.strs, dst) = dt_text.into();
                    }
                    Decode(dst, format, text) => {
                        let format = index(&self.strs, format);
                        let text = index(&self.strs, text);
                        let dt_text = runtime::encoding::decode(format.as_str(), text.as_str());
                        *index_mut(&mut self.strs, dst) = dt_text.into();
                    }
                    Digest(dst, algorithm, text) => {
                        let algorithm = index(&self.strs, algorithm);
                        let text = index(&self.strs, text);
                        let dt_text = runtime::crypto::digest(algorithm.as_str(), text.as_str());
                        *index_mut(&mut self.strs, dst) = dt_text.into();
                    }
                    Escape(dst, format, text) => {
                        let format = index(&self.strs, format);
                        let text = index(&self.strs, text);
                        let escaped_text = text.escape(format);
                        *index_mut(&mut self.strs, dst) = escaped_text;
                    }
                    Hmac(dst, algorithm, key, text) => {
                        let algorithm = index(&self.strs, algorithm);
                        let key = index(&self.strs, key);
                        let text = index(&self.strs, text);
                        let dt_text = runtime::crypto::hmac(algorithm.as_str(), key.as_str(), text.as_str());
                        *index_mut(&mut self.strs, dst) = dt_text.into();
                    }
                    Jwt(dst, algorithm, key, payload) => {
                        let algorithm = index(&self.strs, algorithm);
                        let key = index(&self.strs, key);
                        let payload = self.get(*payload);
                        let token = runtime::crypto::jwt(algorithm.as_str(), key.as_str(), payload);
                        *index_mut(&mut self.strs, dst) = token.into();
                    }
                    Dejwt(dst, key, token) => {
                        let key = index(&self.strs, key);
                        let token = index(&self.strs, token);
                        let res = runtime::crypto::dejwt(key.as_str(), token.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Encrypt(dst, mode, plain_text, key) => {
                        let mode = index(&self.strs, mode);
                        let plain_text = index(&self.strs, plain_text);
                        let key = index(&self.strs, key);
                        let encrypted_text = runtime::crypto::encrypt(mode.as_str(), plain_text.as_str(), key.as_str());
                        *index_mut(&mut self.strs, dst) = encrypted_text.into();
                    }
                    Decrypt(dst, mode, encrypted_text, key) => {
                        let mode = index(&self.strs, mode);
                        let encrypted_text = index(&self.strs, encrypted_text);
                        let key = index(&self.strs, key);
                        let plain_text = runtime::crypto::decrypt(mode.as_str(), encrypted_text.as_str(), key.as_str());
                        *index_mut(&mut self.strs, dst) = plain_text.into();
                    }
                    Strftime(dst, format, timestamp) => {
                        let format = index(&self.strs, format);
                        let tt: i64 = *self.get(*timestamp);
                        let dt_text = runtime::date_time::strftime(format.as_str(), tt);
                        *index_mut(&mut self.strs, dst) = dt_text.into();
                    }
                    Mktime(dst, date_time_text, timezone) => {
                        let dt_text = index(&self.strs, date_time_text);
                        let dt_timezone: i64 = *self.get(*timezone);
                        let result = runtime::date_time::mktime(dt_text.as_str(), dt_timezone);
                        let ir = *dst;
                        *self.get_mut(ir) = result as Int;
                    }
                    Duration(dst, expr) => {
                        let expr = index(&self.strs, expr);
                        let result = runtime::date_time::duration(expr.as_str());
                        let ir = *dst;
                        *self.get_mut(ir) = result as Int;
                    }
                    MkBool(dst, text) => {
                        let text = index(&self.strs, text);
                        let result = runtime::math_util::mkbool(text.as_str());
                        let ir = *dst;
                        *self.get_mut(ir) = result as Int;
                    }
                    MkPassword(dst, len) => {
                        let len: i64 = *self.get(*len);
                        let password = runtime::string_util::generate_password(len as usize);
                        *index_mut(&mut self.strs, dst) = password.into();
                    }
                    Fend(dst, src) => {
                        let res = index(&self.strs, src).fend();
                        *index_mut(&mut self.strs, dst) = res;
                    }
                    Url(dst, src) => {
                        let res = index(&self.strs, src).url();
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Pairs(dst, src, pair_sep, kv_sep) => {
                        let src = index(&self.strs, src);
                        let pair_sep = index(&self.strs, pair_sep);
                        let kv_sep = index(&self.strs, kv_sep);
                        let res = runtime::string_util::pairs(src.as_str(), pair_sep.as_str(), kv_sep.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Parse(dst, text, template) => {
                        let text = index(&self.strs, text);
                        let template = index(&self.strs, template);
                        let res = runtime::string_util::parse(text.as_str(), template.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    RegexParse(dst, text, template) => {
                        let text = index(&self.strs, text);
                        let template = index(&self.strs, template);
                        let res = runtime::string_util::rparse(text.as_str(), template.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Record(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::string_util::record(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Message(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::string_util::message(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    SemVer(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::math_util::semver(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Path(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::os_util::path(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    DataUrl(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::encoding::data_url(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    DateTime(dst, timestamp) => {
                        let timestamp = index(&self.strs, timestamp);
                        let result = runtime::date_time::datetime(timestamp.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = result;
                    }
                    Shlex(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::math_util::shlex(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Tuple(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::math_util::tuple(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Flags(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::math_util::flags(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    ParseArray(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::math_util::parse_array(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Hex2Rgb(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::math_util::hex2rgb(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Rgb2Hex(dst, red, green, blue) => {
                        let red: i64 = *self.get(*red);
                        let green: i64 = *self.get(*green);
                        let blue: i64 = *self.get(*blue);
                        let res = runtime::math_util::rgb2hex(red, green, blue);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(res);
                    }
                    Variant(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::math_util::variant(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Func(dst, text) => {
                        let text = index(&self.strs, text);
                        let res = runtime::string_util::func(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    HttpGet(dst, url, headers) => {
                        let url = index(&self.strs, url);
                        let headers = self.get(*headers);
                        let res = runtime::network::http_get(url.as_str(), headers);
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    HttpPost(dst, url, headers, body) => {
                        let url = index(&self.strs, url);
                        let headers = self.get(*headers);
                        let body = index(&self.strs, body);
                        let res = runtime::network::http_post(url.as_str(), headers, body);
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    S3Get(dst, bucket, object_name) => {
                        let bucket = index(&self.strs, bucket);
                        let object_name = index(&self.strs, object_name);
                        let body = runtime::s3::get_object(bucket.as_str(), object_name.as_str()).unwrap();
                        *index_mut(&mut self.strs, dst) = Str::from(body);
                    }
                    S3Put(dst, bucket, object_name, body) => {
                        let bucket = index(&self.strs, bucket);
                        let object_name = index(&self.strs, object_name);
                        let body = index(&self.strs, body);
                        let etag = runtime::s3::put_object(bucket.as_str(), object_name.as_str(), body.as_str()).unwrap().etag;
                        *index_mut(&mut self.strs, dst) = Str::from(etag);
                    }
                    FromJson(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::json::from_json(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    MapIntIntToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_int_int_to_json(arr));
                    }
                    MapIntFloatToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_int_float_to_json(arr));
                    }
                    MapIntStrToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_int_str_to_json(arr));
                    }
                    MapStrIntToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_str_int_to_json(arr));
                    }
                    MapStrFloatToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_str_float_to_json(arr));
                    }
                    MapStrStrToJson(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::map_str_str_to_json(arr));
                    }
                    StrToJson(dst, text) => {
                        let text = self.get(*text);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::json::str_to_json(text.as_str()));
                    }
                    IntToJson(dst, num) => {
                        let num = *self.get(*num);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(num.to_string());
                    }
                    FloatToJson(dst, num) => {
                        let num = *self.get(*num);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(num.to_string());
                    }
                    NullToJson(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from("null");
                    }
                    DumpMapIntInt(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapIntInt: {}", runtime::json::map_int_int_to_json(arr));
                    }
                    DumpMapIntFloat(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapIntFloat: {}", runtime::json::map_int_float_to_json(arr));
                    }
                    DumpMapIntStr(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapIntStr: {}", runtime::json::map_int_str_to_json(arr));
                    }
                    DumpMapStrInt(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapStrInt: {}", runtime::json::map_str_int_to_json(arr));
                    }
                    DumpMapStrFloat(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapStrFloat: {}", runtime::json::map_str_float_to_json(arr));
                    }
                    DumpMapStrStr(arr) => {
                        let arr = self.get(*arr);
                        eprintln!("MapStrStr: {}", runtime::json::map_str_str_to_json(arr));
                    }
                    DumpStr(text) => {
                        let text = self.get(*text);
                        eprintln!("Str: {}", text.as_str());
                    }
                    DumpInt(num) => {
                        let num = *self.get(*num);
                        eprintln!("Int: {}", num);
                    }
                    DumpFloat(num) => {
                        let num = *self.get(*num);
                        eprintln!("Float: {}", num);
                    }
                    DumpNull() => {
                        eprintln!("Null");
                    }
                    MapIntIntAsort(dst, arr, target) => {
                        let arr = self.get(*arr);
                        let target = self.get(*target);
                        runtime::math_util::map_int_int_asort(arr, target);
                        let dst = *dst;
                        *self.get_mut(dst) = arr.len() as Int;
                    }
                    MapIntFloatAsort(dst, arr, target) => {
                        let arr = self.get(*arr);
                        let target = self.get(*target);
                        runtime::math_util::map_int_float_asort(arr, target);
                        let dst = *dst;
                        *self.get_mut(dst) = arr.len() as Int;
                    }
                    MapIntStrAsort(dst, arr, target) => {
                        let arr = self.get(*arr);
                        let target = self.get(*target);
                        runtime::math_util::map_int_str_asort(arr, target);
                        let dst = *dst;
                        *self.get_mut(dst) = arr.len() as Int;
                    }
                    MapIntIntJoin(dst, arr, sep) => {
                        let arr = self.get(*arr);
                        let sep = self.get(*sep);
                        let value = runtime::math_util::map_int_int_join(arr, sep.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(value);
                    }
                    MapIntFloatJoin(dst, arr, sep) => {
                        let arr = self.get(*arr);
                        let sep = self.get(*sep);
                        let value = runtime::math_util::map_int_float_join(arr, sep.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(value);
                    }
                    MapIntStrJoin(dst, arr, sep) => {
                        let arr = self.get(*arr);
                        let sep = self.get(*sep);
                        let value = runtime::math_util::map_int_str_join(arr, sep.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(value);
                    }
                    MapIntIntMax(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_int_max(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntFloatMax(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_float_max(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntIntMin(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_int_min(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntFloatMin(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_float_min(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntIntSum(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_int_sum(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntFloatSum(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_float_sum(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntIntMean(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_int_mean(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    MapIntFloatMean(dst, arr) => {
                        let arr = self.get(*arr);
                        let value = runtime::math_util::map_int_float_mean(arr);
                        let dst = *dst;
                        *self.get_mut(dst) = value;
                    }
                    FromCsv(dst, src) => {
                        let src = index(&self.strs, src);
                        let res = runtime::csv::from_csv(src.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    MapIntIntToCsv(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::csv::map_int_int_to_csv(arr));
                    }
                    MapIntFloatToCsv(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::csv::map_int_float_to_csv(arr));
                    }
                    MapIntStrToCsv(dst, arr) => {
                        let arr = self.get(*arr);
                        let dst = *dst;
                        *self.get_mut(dst) = Str::from(runtime::csv::map_int_str_to_csv(arr));
                    }
                    KvGet(dst, namespace, key) => {
                        let namespace = index(&self.strs, namespace);
                        let key = index(&self.strs, key);
                        let value = runtime::kv::kv_get(namespace.as_str(), key.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(value);
                    }
                    KvPut(namespace, key, value) => {
                        let namespace = index(&self.strs, namespace);
                        let key = index(&self.strs, key);
                        let value = index(&self.strs, value);
                        runtime::kv::kv_put(namespace.as_str(), key.as_str(), value.as_str());
                    }
                    KvDelete(namespace, key) => {
                        let namespace = index(&self.strs, namespace);
                        let key = index(&self.strs, key);
                        runtime::kv::kv_delete(namespace.as_str(), key.as_str());
                    }
                    KvClear(namespace) => {
                        let namespace = index(&self.strs, namespace);
                        runtime::kv::kv_clear(namespace.as_str());
                    }
                    ReadAll(dst, path) => {
                        let path = index(&self.strs, path);
                        let value = runtime::string_util::read_all(path.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(value);
                    }
                    WriteAll(path, content) => {
                        let path = index(&self.strs, path);
                        let content = index(&self.strs, content);
                        runtime::string_util::write_all(path.as_str(), content.as_str());
                    }
                    LogDebug(message) => {
                        let file_name = &self.core.vars.filename;
                        let message = index(&self.strs, message);
                        runtime::logging::log_debug(file_name.as_str(), message.as_str());
                    }
                    LogInfo(message) => {
                        let file_name = &self.core.vars.filename;
                        let message = index(&self.strs, message);
                        runtime::logging::log_info(file_name.as_str(), message.as_str());
                    }
                    LogWarn(message) => {
                        let file_name = &self.core.vars.filename;
                        let message = index(&self.strs, message);
                        runtime::logging::log_warn(file_name.as_str(), message.as_str());
                    }
                    LogError(message) => {
                        let file_name = &self.core.vars.filename;
                        let message = index(&self.strs, message);
                        runtime::logging::log_error(file_name.as_str(), message.as_str());
                    }
                    SqliteQuery(dst, db_path, sql) => {
                        let db_path = index(&self.strs, db_path);
                        let sql = index(&self.strs, sql);
                        let res = runtime::sqlite::sqlite_query(db_path.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    SqliteExecute(dst, db_path, sql) => {
                        let db_path = index(&self.strs, db_path);
                        let sql = index(&self.strs, sql);
                        let res = runtime::sqlite::sqlite_execute(db_path.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    LibsqlQuery(dst, db_path, sql) => {
                        let db_path = index(&self.strs, db_path);
                        let sql = index(&self.strs, sql);
                        let res = runtime::libsql::libsql_query(db_path.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    LibsqlExecute(dst, db_path, sql) => {
                        let db_path = index(&self.strs, db_path);
                        let sql = index(&self.strs, sql);
                        let res = runtime::libsql::libsql_execute(db_path.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    MysqlQuery(dst, db_url, sql) => {
                        let db_url = index(&self.strs, db_url);
                        let sql = index(&self.strs, sql);
                        let res = runtime::mysql::mysql_query(db_url.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    MysqlExecute(dst, db_url, sql) => {
                        let db_url = index(&self.strs, db_url);
                        let sql = index(&self.strs, sql);
                        let res = runtime::mysql::mysql_execute(db_url.as_str(), sql.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Publish(namespace, body) => {
                        let namespace = index(&self.strs, namespace);
                        let body = index(&self.strs, body);
                        runtime::network::publish(namespace.as_str(), body.as_str());
                    }
                    BloomFilterInsert(item, group) => {
                        let item = index(&self.strs, item);
                        let group = index(&self.strs, group);
                        runtime::encoding::bf_insert(item.as_str(), group.as_str());
                    }
                    BloomFilterContains(dst, item, group) => {
                        let item = index(&self.strs, item);
                        let group = index(&self.strs, group);
                        let res = runtime::encoding::bf_icontains(item.as_str(), group.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    BloomFilterContainsWithInsert(dst, item, group) => {
                        let item = index(&self.strs, item);
                        let group = index(&self.strs, group);
                        let res = runtime::encoding::bf_contains(item.as_str(), group.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Fake(dst, data, locale) => {
                        let data = index(&self.strs, data);
                        let locale = index(&self.strs, locale);
                        let res = runtime::faker::fake(data.as_str(), locale.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(res);
                    }
                    Min(dst, first, second, third) => {
                        let num1 = index(&self.strs, first);
                        let num2 = index(&self.strs, second);
                        let num3 = index(&self.strs, third);
                        let res = runtime::math_util::min(num1.as_str(), num2.as_str(), num3.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(res);
                    }
                    Max(dst, first, second, third) => {
                        let num1 = index(&self.strs, first);
                        let num2 = index(&self.strs, second);
                        let num3 = index(&self.strs, third);
                        let res = runtime::math_util::max(num1.as_str(), num2.as_str(), num3.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(res);
                    }
                    Seq(dst, start, step, end) => {
                        let start: Float = *self.get(*start);
                        let step: Float = *self.get(*step);
                        let end: Float = *self.get(*end);
                        let res = runtime::math_util::seq(start, step, end);
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Uniq(dst, src, param) => {
                        let src = self.get(*src);
                        let param = index(&self.strs, param);
                        let res = runtime::math_util::uniq(src, param.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Trim(dst, src, pat) => {
                        let src = index(&self.strs, src);
                        let pat = index(&self.strs, pat);
                        let dt_text = src.trim(pat);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    Truncate(dst, src, len, place_holder) => {
                        let src = index(&self.strs, src);
                        let len = *self.get(*len);
                        let place_holder = index(&self.strs, place_holder);
                        let truncated_text = src.truncate(len, &place_holder);
                        *index_mut(&mut self.strs, dst) = truncated_text;
                    }
                    Strtonum(dst, text) => {
                        let text = index(&self.strs, text);
                        let num = runtime::math_util::strtonum(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = num;
                    }
                    FormatBytes(dst, size) => {
                        let size = *self.get(*size);
                        let text = runtime::math_util::format_bytes(size);
                        *index_mut(&mut self.strs, dst) = Str::from(text);
                    }
                    ToBytes(dst, text) => {
                        let text = index(&self.strs, text);
                        let size = runtime::math_util::to_bytes(text.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = size;
                    }
                    StartsWith(dst, text, prefix) => {
                        let text = index(&self.strs, text);
                        let prefix = index(&self.strs, prefix);
                        let res = if !text.is_empty() && !prefix.is_empty()
                            && text.as_str().starts_with(prefix.as_str()) {
                            1
                        } else {
                            0
                        };
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    EndsWith(dst, text, suffix) => {
                        let text = index(&self.strs, text);
                        let suffix = index(&self.strs, suffix);
                        let res = if !text.is_empty() && !suffix.is_empty()
                            && text.as_str().ends_with(suffix.as_str()) {
                            1
                        } else {
                            0
                        };
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    TextContains(dst, text, child) => {
                        let text = index(&self.strs, text);
                        let child = index(&self.strs, child);
                        let res = if !text.is_empty() && !child.is_empty()
                            && text.as_str().contains(child.as_str()) {
                            1
                        } else {
                            0
                        };
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Capitalize(dst, text) => {
                        let dt_text = index(&self.strs, text).capitalize();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    UnCapitalize(dst, text) => {
                        let dt_text = index(&self.strs, text).uncapitalize();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    CamelCase(dst, text) => {
                        let dt_text = index(&self.strs, text).camel_case();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    KebabCase(dst, text) => {
                        let dt_text = index(&self.strs, text).kebab_case();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    SnakeCase(dst, text) => {
                        let dt_text = index(&self.strs, text).snake_case();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    TitleCase(dst, text) => {
                        let dt_text = index(&self.strs, text).title_case();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    PadLeft(dst, text, len, pad) => {
                        let text = index(&self.strs, text);
                        let len: Int = *self.get(*len);
                        let pad = index(&self.strs, pad);
                        let dt_text = runtime::string_util::pad_left(text.as_str(), len as usize, pad.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(dt_text);
                    }
                    PadRight(dst, text, len, pad) => {
                        let text = index(&self.strs, text);
                        let len: Int = *self.get(*len);
                        let pad = index(&self.strs, pad);
                        let dt_text = runtime::string_util::pad_right(text.as_str(), len as usize, pad.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(dt_text);
                    }
                    PadBoth(dst, text, len, pad) => {
                        let text = index(&self.strs, text);
                        let len: Int = *self.get(*len);
                        let pad = index(&self.strs, pad);
                        let dt_text = runtime::string_util::pad_both(text.as_str(), len as usize, pad.as_str());
                        *index_mut(&mut self.strs, dst) = Str::from(dt_text);
                    }
                    StrCmp(dst, text1, text2) => {
                        let text1 = index(&self.strs, text1);
                        let text2 = index(&self.strs, text2);
                        let res = runtime::string_util::strcmp(text1.as_str(), text2.as_str());
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    Mask(dst, text) => {
                        let dt_text = index(&self.strs, text).mask();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    Repeat(dst, text, n) => {
                        let n: Int = *self.get(*n);
                        let dt_text = index(&self.strs, text).repeat(n);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    DefaultIfEmpty(dst, text, default_value) => {
                        let default_value = self.get(*default_value);
                        let dt_text = index(&self.strs, text).default_if_empty(default_value);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    AppendIfMissing(dst, text, suffix) => {
                        let suffix = self.get(*suffix);
                        let dt_text = index(&self.strs, text).append_if_missing(suffix);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    PrependIfMissing(dst, text, prefix) => {
                        let prefix = self.get(*prefix);
                        let dt_text = index(&self.strs, text).prepend_if_missing(prefix);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    RemoveIfBegin(dst, text, prefix) => {
                        let prefix = self.get(*prefix);
                        let dt_text = index(&self.strs, text).remove_if_begin(prefix);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    RemoveIfEnd(dst, text, suffix) => {
                        let suffix = self.get(*suffix);
                        let dt_text = index(&self.strs, text).remove_if_end(suffix);
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    Quote(dst, text) => {
                        let dt_text = index(&self.strs, text).quote();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    DoubleQuote(dst, text) => {
                        let dt_text = index(&self.strs, text).double_quote();
                        *index_mut(&mut self.strs, dst) = dt_text;
                    }
                    Words(dst, text) => {
                        let res = index(&self.strs, text).words();
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    TypeOfArray(dst) => {
                        *index_mut(&mut self.strs, dst) = Str::from("array");
                    }
                    TypeOfNumber(dst) => {
                        *index_mut(&mut self.strs, dst) = Str::from("number");
                    }
                    TypeOfString(dst) => {
                        *index_mut(&mut self.strs, dst) = Str::from("string");
                    }
                    TypeOfUnassigned(dst) => {
                        *index_mut(&mut self.strs, dst) = Str::from("unassigned");
                    }
                    IsArrayTrue(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 1;
                    }
                    IsArrayFalse(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 0;
                    }
                    IsIntTrue(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 1;
                    }
                    IsIntFalse(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 0;
                    }
                    IsStrInt(dst, text) => {
                        let text = index(&self.strs, text);
                        let dst = *dst;
                        if runtime::math_util::is_str_int(text.as_str()) {
                            *self.get_mut(dst) = 1;
                        } else {
                            *self.get_mut(dst) = 0;
                        }
                    }
                    IsNumTrue(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 1;
                    }
                    IsNumFalse(dst) => {
                        let dst = *dst;
                        *self.get_mut(dst) = 0;
                    }
                    IsStrNum(dst, text) => {
                        let text = index(&self.strs, text);
                        let dst = *dst;
                        if runtime::math_util::is_str_num(text.as_str()) {
                            *self.get_mut(dst) = 1;
                        } else {
                            *self.get_mut(dst) = 0;
                        }
                    }
                    IsFormat(dst, format, text) => {
                        let format = index(&self.strs, format);
                        let text = index(&self.strs, text);
                        let dst = *dst;
                        *self.get_mut(dst) = runtime::string_util::is_format(format.as_str(), text.as_str());
                    }
                    StrToInt(ir, sr) => {
                        let sr = index(&self.strs, sr);
                        let num = runtime::math_util::strtoint(sr.as_str());
                        let ir = *ir;
                        *self.get_mut(ir) = num;
                    }
                    HexStrToInt(ir, sr) => {
                        let i = self.get(*sr).with_bytes(runtime::hextoi);
                        let ir = *ir;
                        *self.get_mut(ir) = i;
                    }
                    StrToFloat(fr, sr) => {
                        let sr = index(&self.strs, sr);
                        let num = runtime::math_util::strtonum(sr.as_str());
                        let fr = *fr;
                        *self.get_mut(fr) = num;
                    }
                    FloatToInt(ir, fr) => {
                        let i = runtime::convert::<_, Int>(*self.get(*fr));
                        let ir = *ir;
                        *self.get_mut(ir) = i;
                    }
                    IntToFloat(fr, ir) => {
                        let f = runtime::convert::<_, Float>(*self.get(*ir));
                        let fr = *fr;
                        *self.get_mut(fr) = f;
                    }
                    AddInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l + r;
                    }
                    AddFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l + r;
                    }
                    MulInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l * r;
                    }
                    MulFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l * r;
                    }
                    MinusInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l - r;
                    }
                    MinusFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l - r;
                    }
                    ModInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l % r;
                    }
                    ModFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l % r;
                    }
                    Div(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l / r;
                    }
                    Pow(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = l.powf(r);
                    }
                    Not(res, ir) => {
                        let res = *res;
                        let i = *self.get(*ir);
                        *self.get_mut(res) = (i == 0) as Int;
                    }
                    NotStr(res, sr) => {
                        let res = *res;
                        let sr = *sr;
                        let is_empty = self.get(sr).with_bytes(|bs| bs.is_empty());
                        *self.get_mut(res) = is_empty as Int;
                    }
                    NegInt(res, ir) => {
                        let res = *res;
                        let i = *self.get(*ir);
                        *self.get_mut(res) = -i;
                    }
                    NegFloat(res, fr) => {
                        let res = *res;
                        let f = *self.get(*fr);
                        *self.get_mut(res) = -f;
                    }
                    Float1(ff, dst, src) => {
                        let f = *index(&self.floats, src);
                        let dst = *dst;
                        *self.get_mut(dst) = ff.eval1(f);
                    }
                    Float2(ff, dst, x, y) => {
                        let fx = *index(&self.floats, x);
                        let fy = *index(&self.floats, y);
                        let dst = *dst;
                        *self.get_mut(dst) = ff.eval2(fx, fy);
                    }
                    Int1(bw, dst, src) => {
                        let i = *index(&self.ints, src);
                        let dst = *dst;
                        *self.get_mut(dst) = bw.eval1(i);
                    }
                    Int2(bw, dst, x, y) => {
                        let ix = *index(&self.ints, x);
                        let iy = *index(&self.ints, y);
                        let dst = *dst;
                        *self.get_mut(dst) = bw.eval2(ix, iy);
                    }
                    Rand(dst) => {
                        let res: f64 = self.core.rng.gen_range(0.0..=1.0);
                        *index_mut(&mut self.floats, dst) = res;
                    }
                    Srand(res, seed) => {
                        let old_seed = self.core.reseed(*index(&self.ints, seed) as u64);
                        *index_mut(&mut self.ints, res) = old_seed as Int;
                    }
                    ReseedRng(res) => {
                        *index_mut(&mut self.ints, res) = self.core.reseed_random() as Int;
                    }
                    StartsWithConst(res, s, bs) => {
                        let s_bytes = unsafe { &*index(&self.strs, s).get_bytes() };
                        *index_mut(&mut self.ints, res) =
                            (bs.len() <= s_bytes.len() && s_bytes[..bs.len()] == **bs) as Int;
                    }
                    Concat(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l).clone();
                        let r = self.get(*r).clone();
                        *self.get_mut(res) = Str::concat(l, r);
                    }
                    Match(res, l, r) => {
                        *index_mut(&mut self.ints, res) = self
                            .core
                            .match_regex(index(&self.strs, l), index(&self.strs, r))?;
                    }
                    IsMatch(res, l, r) => {
                        *index_mut(&mut self.ints, res) = self
                            .core
                            .is_match_regex(index(&self.strs, l), index(&self.strs, r))?
                            as Int;
                    }
                    MatchConst(res, x, pat) => {
                        *index_mut(&mut self.ints, res) =
                            runtime::RegexCache::regex_const_match(pat, index(&self.strs, x))
                                as Int;
                    }
                    IsMatchConst(res, x, pat) => {
                        *index_mut(&mut self.ints, res) =
                            self.core.match_const_regex(index(&self.strs, x), pat)?;
                    }
                    SubstrIndex(res, s, t) => {
                        let res = *res;
                        let s = index(&self.strs, s);
                        let t = index(&self.strs, t);
                        *self.get_mut(res) = runtime::string_search::index_substr(t, s);
                    }
                    SubstrLastIndex(res, s, t) => {
                        let res = *res;
                        let s = index(&self.strs, s);
                        let t = index(&self.strs, t);
                        *self.get_mut(res) = runtime::string_search::last_index_substr(t, s);
                    }
                    LenStr(res, s) => {
                        let res = *res;
                        let s = *s;
                        // TODO consider doing a with_str here or enforce elsewhere that strings
                        // cannot exceed u32::max.
                        let len = self.get(s).len();
                        *self.get_mut(res) = len as Int;
                    }
                    Sub(res, pat, s, in_s) => {
                        let (subbed, new) = {
                            let pat = index(&self.strs, pat);
                            let s = index(&self.strs, s);
                            let in_s = index(&self.strs, in_s);
                            self.core
                                .regexes
                                .with_regex(pat, |re| in_s.subst_first(re, s))?
                        };
                        *index_mut(&mut self.strs, in_s) = subbed;
                        *index_mut(&mut self.ints, res) = new as Int;
                    }
                    GSub(res, pat, s, in_s) => {
                        let (subbed, subs_made) = {
                            let pat = index(&self.strs, pat);
                            let s = index(&self.strs, s);
                            let in_s = index(&self.strs, in_s);
                            self.core
                                .regexes
                                .with_regex(pat, |re| in_s.subst_all(re, s))?
                        };
                        *index_mut(&mut self.strs, in_s) = subbed;
                        *index_mut(&mut self.ints, res) = subs_made;
                    }
                    GenSubDynamic(res, pat, s, how, in_s) => {
                        let subbed = {
                            let pat = index(&self.strs, pat);
                            let s = index(&self.strs, s);
                            let how = index(&self.strs, how);
                            let in_s = index(&self.strs, in_s);
                            self.core
                                .regexes
                                .with_regex(pat, |re| in_s.gen_subst_dynamic(re, s, how))?
                        };
                        *index_mut(&mut self.strs, res) = subbed;
                    }
                    EscapeCSV(res, s) => {
                        *index_mut(&mut self.strs, res) = {
                            let s = index(&self.strs, s);
                            runtime::escape_csv(s)
                        };
                    }
                    EscapeTSV(res, s) => {
                        *index_mut(&mut self.strs, res) = {
                            let s = index(&self.strs, s);
                            runtime::escape_tsv(s)
                        };
                    }
                    Substr(res, base, l, r) => {
                        let text = index(&self.strs, base);
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        let sub_str = text.sub_str((l - 1) as usize, r as usize);
                        *index_mut(&mut self.strs, res) = sub_str;
                    }
                    CharAt(dst, text, index) => {
                        let index = *self.get(*index);
                        if index <= 0 {
                            panic!("invalid index for chat_at: {}, should start with 1", index)
                        } else {
                            let text = self.get(*text);
                            let index = (index - 1) as usize;
                            *index_mut(&mut self.strs, dst) = text.char_at(index);
                        }
                    }
                    LastPart(res, s, sep) => {
                        let s = self.get(*s);
                        let sep = self.get(*sep);
                        let last_part = runtime::string_util::last_part(s.as_str(), sep.as_str());
                        *index_mut(&mut self.strs, res) = Str::from(last_part);
                    }
                    LTFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l < r) as Int;
                    }
                    LTInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l < r) as Int;
                    }
                    LTStr(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l);
                        let r = self.get(*r);
                        *self.get_mut(res) = l.with_bytes(|l| r.with_bytes(|r| l < r)) as Int;
                    }
                    GTFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l > r) as Int;
                    }
                    GTInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l > r) as Int;
                    }
                    GTStr(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l);
                        let r = self.get(*r);
                        *self.get_mut(res) = l.with_bytes(|l| r.with_bytes(|r| l > r)) as Int;
                    }
                    LTEFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l <= r) as Int;
                    }
                    LTEInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l <= r) as Int;
                    }
                    LTEStr(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l);
                        let r = self.get(*r);
                        *self.get_mut(res) = l.with_bytes(|l| r.with_bytes(|r| l <= r)) as Int;
                    }
                    GTEFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l >= r) as Int;
                    }
                    GTEInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l >= r) as Int;
                    }
                    GTEStr(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l);
                        let r = self.get(*r);
                        *self.get_mut(res) = l.with_bytes(|l| r.with_bytes(|r| l >= r)) as Int;
                    }
                    EQFloat(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l == r) as Int;
                    }
                    EQInt(res, l, r) => {
                        let res = *res;
                        let l = *self.get(*l);
                        let r = *self.get(*r);
                        *self.get_mut(res) = (l == r) as Int;
                    }
                    EQStr(res, l, r) => {
                        let res = *res;
                        let l = self.get(*l);
                        let r = self.get(*r);
                        *self.get_mut(res) = (l == r) as Int;
                    }
                    SetColumn(dst, src) => {
                        let col = *self.get(*dst);
                        let v = index(&self.strs, src);
                        self.line
                            .set_col(col, v, &self.core.vars.ofs, &mut self.core.regexes)?;
                    }
                    GetColumn(dst, src) => {
                        let col = *self.get(*src);
                        let dst = *dst;
                        let res = self.line.get_col(
                            col,
                            &self.core.vars.fs,
                            &self.core.vars.ofs,
                            &mut self.core.regexes,
                        )?;
                        *self.get_mut(dst) = res;
                    }
                    JoinCSV(dst, start, end) => {
                        let nf = self.line.nf(&self.core.vars.fs, &mut self.core.regexes)?;
                        *index_mut(&mut self.strs, dst) = {
                            let start = *index(&self.ints, start);
                            let end = *index(&self.ints, end);
                            self.line.join_cols(start, end, &",".into(), nf, |s| {
                                runtime::escape_csv(&s)
                            })?
                        };
                    }
                    JoinTSV(dst, start, end) => {
                        let nf = self.line.nf(&self.core.vars.fs, &mut self.core.regexes)?;
                        *index_mut(&mut self.strs, dst) = {
                            let start = *index(&self.ints, start);
                            let end = *index(&self.ints, end);
                            self.line.join_cols(start, end, &"\t".into(), nf, |s| {
                                runtime::escape_tsv(&s)
                            })?
                        };
                    }
                    JoinColumns(dst, start, end, sep) => {
                        let nf = self.line.nf(&self.core.vars.fs, &mut self.core.regexes)?;
                        *index_mut(&mut self.strs, dst) = {
                            let sep = index(&self.strs, sep);
                            let start = *index(&self.ints, start);
                            let end = *index(&self.ints, end);
                            self.line.join_cols(start, end, sep, nf, |s| s)?
                        };
                    }
                    ToUpperAscii(dst, src) => {
                        let res = index(&self.strs, src).to_upper_ascii();
                        *index_mut(&mut self.strs, dst) = res;
                    }
                    ToLowerAscii(dst, src) => {
                        let res = index(&self.strs, src).to_lower_ascii();
                        *index_mut(&mut self.strs, dst) = res;
                    }
                    SplitInt(flds, to_split, arr, pat) => {
                        // Index manually here to defeat the borrow checker.
                        let to_split = index(&self.strs, to_split);
                        let arr = index(&self.maps_int_str, arr);
                        let pat = index(&self.strs, pat);
                        self.core.regexes.split_regex_intmap(pat, to_split, arr)?;
                        let res = arr.len() as Int;
                        let flds = *flds;
                        *self.get_mut(flds) = res;
                    }
                    SplitStr(flds, to_split, arr, pat) => {
                        // Very similar to above
                        let to_split = index(&self.strs, to_split);
                        let arr = index(&self.maps_str_str, arr);
                        let pat = index(&self.strs, pat);
                        self.core.regexes.split_regex_strmap(pat, to_split, arr)?;
                        let res = arr.len() as Int;
                        let flds = *flds;
                        *self.get_mut(flds) = res;
                    }
                    Sprintf { dst, fmt, args } => {
                        debug_assert_eq!(scratch.len(), 0);
                        for a in args.iter() {
                            scratch.push(self.format_arg(*a)?);
                        }
                        use runtime::str_impl::DynamicBuf;
                        let fmt_str = index(&self.strs, fmt);
                        let mut buf = DynamicBuf::new(0);
                        fmt_str
                            .with_bytes(|bs| runtime::printf::printf(&mut buf, bs, &scratch[..]))?;
                        scratch.clear();
                        let res = buf.into_str();
                        let dst = *dst;
                        *self.get_mut(dst) = res;
                    }
                    PrintAll { output, args } => {
                        let mut scratch_strs =
                            smallvec::SmallVec::<[&Str; 4]>::with_capacity(args.len());
                        for a in args {
                            scratch_strs.push(index(&self.strs, a));
                        }
                        let res = if let Some((out_path_reg, fspec)) = output {
                            let out_path = index(&self.strs, out_path_reg);
                            self.core
                                .write_files
                                .write_all(&scratch_strs[..], Some((out_path, *fspec)))
                        } else {
                            self.core.write_files.write_all(&scratch_strs[..], None)
                        };
                        if res.is_err() {
                            return Ok(0);
                        }
                    }
                    Printf { output, fmt, args } => {
                        debug_assert_eq!(scratch.len(), 0);
                        for a in args.iter() {
                            scratch.push(self.format_arg(*a)?);
                        }
                        let fmt_str = index(&self.strs, fmt);
                        let res = if let Some((out_path_reg, fspec)) = output {
                            let out_path = index(&self.strs, out_path_reg);
                            self.core.write_files.printf(
                                Some((out_path, *fspec)),
                                fmt_str,
                                &scratch[..],
                            )
                        } else {
                            // print to stdout.
                            self.core.write_files.printf(None, fmt_str, &scratch[..])
                        };
                        if res.is_err() {
                            return Ok(0);
                        }
                        scratch.clear();
                    }
                    Close(file) => {
                        let file = index(&self.strs, file);
                        // NB this may create an unused entry in write_files. It would not be
                        // terribly difficult to optimize the close path to include an existence
                        // check first.
                        self.core.write_files.close(file)?;
                        self.read_files.close(file);
                    }
                    RunCmd(dst, cmd) => {
                        *index_mut(&mut self.ints, dst) =
                            index(&self.strs, cmd).with_bytes(runtime::run_command);
                    }
                    Exit(code) => return Ok(*index(&self.ints, code) as i32),
                    Lookup {
                        map_ty,
                        dst,
                        map,
                        key,
                    } => self.lookup(*map_ty, *dst, *map, *key),
                    Contains {
                        map_ty,
                        dst,
                        map,
                        key,
                    } => self.contains(*map_ty, *dst, *map, *key),
                    Delete { map_ty, map, key } => self.delete(*map_ty, *map, *key),
                    Clear { map_ty, map } => self.clear(*map_ty, *map),
                    Len { map_ty, map, dst } => self.len(*map_ty, *map, *dst),
                    Store {
                        map_ty,
                        map,
                        key,
                        val,
                    } => self.store_map(*map_ty, *map, *key, *val),
                    IncInt {
                        map_ty,
                        map,
                        key,
                        by,
                        dst,
                    } => self.inc_map_int(*map_ty, *map, *key, *by, *dst),
                    IncFloat {
                        map_ty,
                        map,
                        key,
                        by,
                        dst,
                    } => self.inc_map_float(*map_ty, *map, *key, *by, *dst),
                    LoadVarStr(dst, var) => {
                        let s = self.core.vars.load_str(*var)?;
                        let dst = *dst;
                        *self.get_mut(dst) = s;
                    }
                    StoreVarStr(var, src) => {
                        let src = *src;
                        let s = self.get(src).clone();
                        self.core.vars.store_str(*var, s)?;
                    }
                    LoadVarInt(dst, var) => {
                        // If someone explicitly sets NF to a different value, this means we will
                        // ignore it. I think that is fine.
                        if let NF = *var {
                            self.core.vars.nf =
                                self.line.nf(&self.core.vars.fs, &mut self.core.regexes)? as Int;
                        }
                        let i = self.core.vars.load_int(*var)?;
                        let dst = *dst;
                        *self.get_mut(dst) = i;
                    }
                    StoreVarInt(var, src) => {
                        let src = *src;
                        let s = *self.get(src);
                        self.core.vars.store_int(*var, s)?;
                    }
                    LoadVarIntMap(dst, var) => {
                        let arr = self.core.vars.load_intmap(*var)?;
                        let dst = *dst;
                        *self.get_mut(dst) = arr;
                    }
                    StoreVarIntMap(var, src) => {
                        let src = *src;
                        let s = self.get(src).clone();
                        self.core.vars.store_intmap(*var, s)?;
                    }
                    LoadVarStrMap(dst, var) => {
                        let arr = self.core.vars.load_strmap(*var)?;
                        let dst = *dst;
                        *self.get_mut(dst) = arr;
                    }
                    StoreVarStrMap(var, src) => {
                        let src = *src;
                        let s = self.get(src).clone();
                        self.core.vars.store_strmap(*var, s)?;
                    }
                    LoadVarStrStrMap(dst, var) => {
                        let arr = self.core.vars.load_strstrmap(*var)?;
                        let dst = *dst;
                        *self.get_mut(dst) = arr;
                    }
                    StoreVarStrStrMap(var, src) => {
                        let src = *src;
                        let s = self.get(src).clone();
                        self.core.vars.store_strstrmap(*var, s)?;
                    }

                    IterBegin { map_ty, map, dst } => self.iter_begin(*map_ty, *map, *dst),
                    IterHasNext { iter_ty, dst, iter } => self.iter_has_next(*iter_ty, *dst, *iter),
                    IterGetNext { iter_ty, dst, iter } => self.iter_get_next(*iter_ty, *dst, *iter),

                    LoadSlot { ty, dst, slot } => self.load_slot(*ty, *dst, *slot),
                    StoreSlot { ty, src, slot } => self.store_slot(*ty, *src, *slot),
                    Mov(ty, dst, src) => self.mov(*ty, *dst, *src),
                    AllocMap(ty, reg) => self.alloc_map(*ty, *reg),

                    // TODO add error logging for these errors perhaps?
                    ReadErr(dst, file, is_file) => {
                        let dst = *dst;
                        let file = index(&self.strs, file);
                        let res = if *is_file {
                            self.read_files.read_err(file)?
                        } else {
                            self.read_files.read_err_cmd(file)?
                        };
                        *self.get_mut(dst) = res;
                    }
                    NextLine(dst, file, is_file) => {
                        let dst = *dst;
                        let file = index(&self.strs, file);
                        match self.core.regexes.get_line(
                            file,
                            &self.core.vars.rs,
                            &mut self.read_files,
                            *is_file,
                        ) {
                            Ok(l) => *self.get_mut(dst) = l,
                            Err(_) => *self.get_mut(dst) = "".into(),
                        };
                    }
                    ReadErrStdin(dst) => {
                        let dst = *dst;
                        let res = self.read_files.read_err_stdin();
                        *self.get_mut(dst) = res;
                    }
                    NextLineStdin(dst) => {
                        let dst = *dst;
                        let (changed, res) = self
                            .core
                            .regexes
                            .get_line_stdin(&self.core.vars.rs, &mut self.read_files)?;
                        if changed {
                            self.reset_file_vars();
                        }
                        *self.get_mut(dst) = res;
                    }
                    NextLineStdinFused() => {
                        let changed = self.core.regexes.get_line_stdin_reuse(
                            &self.core.vars.rs,
                            &mut self.read_files,
                            &mut self.line,
                        )?;
                        if changed {
                            self.reset_file_vars()
                        }
                    }
                    NextFile() => {
                        self.read_files.next_file()?;
                        self.reset_file_vars();
                    }
                    UpdateUsedFields() => {
                        let fi = &self.core.vars.fi;
                        self.read_files.update_named_columns(fi);
                    }
                    SetFI(key, val) => {
                        let key = *index(&self.ints, key);
                        let val = *index(&self.ints, val);
                        let col = self.line.get_col(
                            key,
                            &self.core.vars.fs,
                            &self.core.vars.ofs,
                            &mut self.core.regexes,
                        )?;
                        self.core.vars.fi.insert(col, val);
                    }
                    JmpIf(cond, lbl) => {
                        let cond = *cond;
                        if *self.get(cond) != 0 {
                            break lbl.0;
                        }
                    }
                    Jmp(lbl) => {
                        break lbl.0;
                    }
                    Push(ty, reg) => self.push_reg(*ty, *reg),
                    Pop(ty, reg) => self.pop_reg(*ty, *reg),
                    Call(func) => {
                        self.stack.push((cur_fn, Label(cur + 1)));
                        cur_fn = *func;
                        instrs = &mut self.instrs[*func];
                        break 0;
                    }
                    Ret => {
                        if let Some((func, Label(inst))) = self.stack.pop() {
                            cur_fn = func;
                            instrs = &mut self.instrs[func];
                            break inst;
                        } else {
                            break 'outer Ok(0);
                        }
                    }
                };
                break cur + 1;
            };
        }
    }
    fn mov(&mut self, ty: Ty, dst: NumTy, src: NumTy) {
        match ty {
            Ty::Int => {
                let src = *index(&self.ints, &src.into());
                *index_mut(&mut self.ints, &dst.into()) = src;
            }
            Ty::Float => {
                let src = *index(&self.floats, &src.into());
                *index_mut(&mut self.floats, &dst.into()) = src;
            }
            Ty::Str => {
                let src = index(&self.strs, &src.into()).clone();
                *index_mut(&mut self.strs, &dst.into()) = src;
            }
            Ty::MapIntInt => {
                let src = index(&self.maps_int_int, &src.into()).clone();
                *index_mut(&mut self.maps_int_int, &dst.into()) = src;
            }
            Ty::MapIntFloat => {
                let src = index(&self.maps_int_float, &src.into()).clone();
                *index_mut(&mut self.maps_int_float, &dst.into()) = src;
            }
            Ty::MapIntStr => {
                let src = index(&self.maps_int_str, &src.into()).clone();
                *index_mut(&mut self.maps_int_str, &dst.into()) = src;
            }
            Ty::MapStrInt => {
                let src = index(&self.maps_str_int, &src.into()).clone();
                *index_mut(&mut self.maps_str_int, &dst.into()) = src;
            }
            Ty::MapStrFloat => {
                let src = index(&self.maps_str_float, &src.into()).clone();
                *index_mut(&mut self.maps_str_float, &dst.into()) = src;
            }
            Ty::MapStrStr => {
                let src = index(&self.maps_str_str, &src.into()).clone();
                *index_mut(&mut self.maps_str_str, &dst.into()) = src;
            }
            Ty::Null | Ty::IterInt | Ty::IterStr => {
                panic!("invalid type for move operation: {:?}", ty)
            }
        }
    }
    fn alloc_map(&mut self, ty: Ty, reg: NumTy) {
        map_regs!(ty, reg, *self.get_mut(reg) = Default::default())
    }
    fn lookup(&mut self, map_ty: Ty, dst: NumTy, map: NumTy, key: NumTy) {
        map_regs!(map_ty, map, key, dst, {
            let res = self.get(map).get(self.get(key));
            *self.get_mut(dst) = res;
        });
    }
    fn contains(&mut self, map_ty: Ty, dst: NumTy, map: NumTy, key: NumTy) {
        let _v = 0u32;
        let dst: Reg<Int> = dst.into();
        map_regs!(map_ty, map, key, _v, {
            let res = self.get(map).contains(self.get(key)) as Int;
            *self.get_mut(dst) = res;
        });
    }
    fn delete(&mut self, map_ty: Ty, map: NumTy, key: NumTy) {
        let _v = 0u32;
        map_regs!(map_ty, map, key, _v, {
            self.get(map).delete(self.get(key))
        });
    }
    fn clear(&mut self, map_ty: Ty, map: NumTy) {
        map_regs!(map_ty, map, self.get(map).clear());
    }

    // Allowing this because it allows for easier use of the map_regs macro.
    #[allow(clippy::clone_on_copy)]
    fn store_map(&mut self, map_ty: Ty, map: NumTy, key: NumTy, val: NumTy) {
        map_regs!(map_ty, map, key, val, {
            let k = self.get(key).clone();
            let v = self.get(val).clone();
            self.get(map).insert(k, v);
        });
    }
    fn inc_map_int(&mut self, map_ty: Ty, map: NumTy, key: NumTy, by: Reg<Int>, dst: NumTy) {
        map_regs!(map_ty, map, key, dst, {
            let k = self.get(key);
            let m = self.get(map);
            let by = *self.get(by);
            let res = m.inc_int(k, by);
            *self.get_mut(dst) = res;
        })
    }
    fn inc_map_float(&mut self, map_ty: Ty, map: NumTy, key: NumTy, by: Reg<Float>, dst: NumTy) {
        map_regs!(map_ty, map, key, dst, {
            let k = self.get(key);
            let m = self.get(map);
            let by = *self.get(by);
            let res = m.inc_float(k, by);
            *self.get_mut(dst) = res;
        })
    }
    fn len(&mut self, map_ty: Ty, map: NumTy, dst: NumTy) {
        let len = map_regs!(map_ty, map, self.get(map).len() as Int);
        *index_mut(&mut self.ints, &dst.into()) = len;
    }
    fn iter_begin(&mut self, map_ty: Ty, map: NumTy, dst: NumTy) {
        let _k = 0u32;
        let _v = 0u32;
        map_regs!(map_ty, map, _k, _v, dst, {
            let iter = self.get(map).to_iter();
            *self.get_mut(dst) = iter;
        })
    }
    fn iter_has_next(&mut self, iter_ty: Ty, dst: NumTy, iter: NumTy) {
        match iter_ty {
            Ty::IterInt => {
                let res = index(&self.iters_int, &iter.into()).has_next() as Int;
                *index_mut(&mut self.ints, &dst.into()) = res;
            }
            Ty::IterStr => {
                let res = index(&self.iters_str, &iter.into()).has_next() as Int;
                *index_mut(&mut self.ints, &dst.into()) = res;
            }
            x => panic!("non-iterator type passed to has_next: {:?}", x),
        }
    }
    fn iter_get_next(&mut self, iter_ty: Ty, dst: NumTy, iter: NumTy) {
        match iter_ty {
            Ty::IterInt => {
                let res = unsafe { *index(&self.iters_int, &iter.into()).get_next() };
                *index_mut(&mut self.ints, &dst.into()) = res;
            }
            Ty::IterStr => {
                let res = unsafe { index(&self.iters_str, &iter.into()).get_next().clone() };
                *index_mut(&mut self.strs, &dst.into()) = res;
            }
            x => panic!("non-iterator type passed to get_next: {:?}", x),
        }
    }
    fn load_slot(&mut self, ty: Ty, dst: NumTy, slot: Int) {
        let slot = slot as usize;
        macro_rules! do_load {
            ($load_meth:tt, $reg_fld:tt) => {
                *index_mut(&mut self.$reg_fld, &dst.into()) = self.core.$load_meth(slot)
            };
        }
        match ty {
            Ty::Int => do_load!(load_int, ints),
            Ty::Float => do_load!(load_float, floats),
            Ty::Str => do_load!(load_str, strs),
            Ty::MapIntInt => do_load!(load_intint, maps_int_int),
            Ty::MapIntFloat => do_load!(load_intfloat, maps_int_float),
            Ty::MapIntStr => do_load!(load_intstr, maps_int_str),
            Ty::MapStrInt => do_load!(load_strint, maps_str_int),
            Ty::MapStrFloat => do_load!(load_strfloat, maps_str_float),
            Ty::MapStrStr => do_load!(load_strstr, maps_str_str),
            Ty::Null | Ty::IterInt | Ty::IterStr => {
                panic!("unexpected operand type to slot operation: {:?}", ty)
            }
        }
    }
    fn store_slot(&mut self, ty: Ty, src: NumTy, slot: Int) {
        let slot = slot as usize;
        macro_rules! do_store {
            ($store_meth:tt, $reg_fld:tt) => {
                self.core
                    .$store_meth(slot, index(&self.$reg_fld, &src.into()).clone())
            };
        }
        match ty {
            Ty::Int => do_store!(store_int, ints),
            Ty::Float => do_store!(store_float, floats),
            Ty::Str => do_store!(store_str, strs),
            Ty::MapIntInt => do_store!(store_intint, maps_int_int),
            Ty::MapIntFloat => do_store!(store_intfloat, maps_int_float),
            Ty::MapIntStr => do_store!(store_intstr, maps_int_str),
            Ty::MapStrInt => do_store!(store_strint, maps_str_int),
            Ty::MapStrFloat => do_store!(store_strfloat, maps_str_float),
            Ty::MapStrStr => do_store!(store_strstr, maps_str_str),
            Ty::Null | Ty::IterInt | Ty::IterStr => panic!("unsupported slot type: {:?}", ty),
        }
    }
    fn push_reg(&mut self, ty: Ty, src: NumTy) {
        match ty {
            Ty::Int => push(&mut self.ints, &src.into()),
            Ty::Float => push(&mut self.floats, &src.into()),
            Ty::Str => push(&mut self.strs, &src.into()),
            Ty::MapIntInt => push(&mut self.maps_int_int, &src.into()),
            Ty::MapIntFloat => push(&mut self.maps_int_float, &src.into()),
            Ty::MapIntStr => push(&mut self.maps_int_str, &src.into()),
            Ty::MapStrInt => push(&mut self.maps_str_int, &src.into()),
            Ty::MapStrFloat => push(&mut self.maps_str_float, &src.into()),
            Ty::MapStrStr => push(&mut self.maps_str_str, &src.into()),
            Ty::Null | Ty::IterInt | Ty::IterStr => {
                panic!("unsupported register type for push operation: {:?}", ty)
            }
        }
    }
    fn pop_reg(&mut self, ty: Ty, dst: NumTy) {
        match ty {
            Ty::Int => *index_mut(&mut self.ints, &dst.into()) = pop(&mut self.ints),
            Ty::Float => *index_mut(&mut self.floats, &dst.into()) = pop(&mut self.floats),
            Ty::Str => *index_mut(&mut self.strs, &dst.into()) = pop(&mut self.strs),
            Ty::MapIntInt => {
                *index_mut(&mut self.maps_int_int, &dst.into()) = pop(&mut self.maps_int_int)
            }
            Ty::MapIntFloat => {
                *index_mut(&mut self.maps_int_float, &dst.into()) = pop(&mut self.maps_int_float)
            }
            Ty::MapIntStr => {
                *index_mut(&mut self.maps_int_str, &dst.into()) = pop(&mut self.maps_int_str)
            }
            Ty::MapStrInt => {
                *index_mut(&mut self.maps_str_int, &dst.into()) = pop(&mut self.maps_str_int)
            }
            Ty::MapStrFloat => {
                *index_mut(&mut self.maps_str_float, &dst.into()) = pop(&mut self.maps_str_float)
            }
            Ty::MapStrStr => {
                *index_mut(&mut self.maps_str_str, &dst.into()) = pop(&mut self.maps_str_str)
            }
            Ty::Null | Ty::IterInt | Ty::IterStr => {
                panic!("unsupported register type for pop operation: {:?}", ty)
            }
        }
    }
}

// TODO: Add a pass that does checking of indexes once.
// That could justify no checking during interpretation.
#[cfg(debug_assertions)]
const CHECKED: bool = true;
#[cfg(not(debug_assertions))]
const CHECKED: bool = false;

#[inline(always)]
pub(crate) fn index<'a, T>(Storage { regs, .. }: &'a Storage<T>, reg: &Reg<T>) -> &'a T {
    if CHECKED {
        &regs[reg.index()]
    } else {
        debug_assert!(reg.index() < regs.len());
        unsafe { regs.get_unchecked(reg.index()) }
    }
}

#[inline(always)]
pub(crate) fn index_mut<'a, T>(
    Storage { regs, .. }: &'a mut Storage<T>,
    reg: &Reg<T>,
) -> &'a mut T {
    if CHECKED {
        &mut regs[reg.index()]
    } else {
        debug_assert!(reg.index() < regs.len());
        unsafe { regs.get_unchecked_mut(reg.index()) }
    }
}

pub(crate) fn push<T: Clone>(s: &mut Storage<T>, reg: &Reg<T>) {
    let v = index(s, reg).clone();
    s.stack.push(v);
}

pub(crate) fn pop<T: Clone>(s: &mut Storage<T>) -> T {
    s.stack.pop().expect("pop must be called on nonempty stack")
}

// Used in benchmarking code.

#[cfg(test)]
impl<T: Default> Storage<T> {
    #[cfg(feature = "unstable")]
    fn reset(&mut self) {
        self.stack.clear();
        for i in self.regs.iter_mut() {
            *i = Default::default();
        }
    }
}

#[cfg(test)]
impl<'a, LR: LineReader> Interp<'a, LR> {
    #[cfg(feature = "unstable")]
    pub(crate) fn reset(&mut self) {
        self.stack = Default::default();
        self.core.vars = Default::default();
        self.line = Default::default();
        self.core.regexes = Default::default();
        self.floats.reset();
        self.ints.reset();
        self.strs.reset();
        self.maps_int_int.reset();
        self.maps_int_float.reset();
        self.maps_int_str.reset();
        self.maps_str_int.reset();
        self.maps_str_float.reset();
        self.maps_str_str.reset();
        self.iters_int.reset();
        self.iters_str.reset();
    }
}
