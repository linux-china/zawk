use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

use crate::builtins::{Bitwise, FloatFunc, Variable};
use crate::common::{FileSpec, NumTy};
use crate::compile::{self, Ty};
use crate::interp::{index, index_mut, Storage};
use crate::runtime::{self, Float, Int, Str, UniqueStr};

use regex::bytes::Regex;

pub(crate) use crate::interp::Interp;

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct Label(pub usize);

impl std::fmt::Debug for Label {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "@{}", self.0)
    }
}

impl From<usize> for Label {
    fn from(u: usize) -> Label {
        Label(u)
    }
}

pub struct Reg<T>(pub u32, pub PhantomData<*const T>);

impl<T> std::fmt::Debug for Reg<T> {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(fmt, "<{}>", self.0)
    }
}

impl<T> From<u32> for Reg<T> {
    fn from(u: u32) -> Reg<T> {
        assert_ne!(u, compile::UNUSED, "creating an unused register");
        assert_ne!(u, compile::NULL_REG, "creating a null register");
        Reg(u, PhantomData)
    }
}
impl<T> Clone for Reg<T> {
    fn clone(&self) -> Reg<T> {
        *self
    }
}
impl<T> Copy for Reg<T> {}
impl<T> Hash for Reg<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}
impl<T> PartialEq for Reg<T> {
    fn eq(&self, other: &Reg<T>) -> bool {
        self.0 == other.0
    }
}
impl<T> Eq for Reg<T> {}
// PhantomData gets in the way here.
unsafe impl<T> Send for Reg<T> {}

#[derive(Debug, Clone)]
pub(crate) enum Instr<'a> {
    // By default, instructions have destination first, and src(s) second.
    StoreConstStr(Reg<Str<'a>>, UniqueStr<'a>),
    StoreConstInt(Reg<Int>, Int),
    StoreConstFloat(Reg<Float>, Float),

    // Conversions
    IntToStr(Reg<Str<'a>>, Reg<Int>),
    FloatToStr(Reg<Str<'a>>, Reg<Float>),
    StrToInt(Reg<Int>, Reg<Str<'a>>),
    HexStrToInt(Reg<Int>, Reg<Str<'a>>),
    FloatToInt(Reg<Int>, Reg<Float>),
    IntToFloat(Reg<Float>, Reg<Int>),
    StrToFloat(Reg<Float>, Reg<Str<'a>>),

    // Assignment
    // Note, for now we do not support iterator moves. Iterators own their own copy of an array,
    // and there is no reason we should be emitting movs for them.
    Mov(Ty, NumTy, NumTy),

    AllocMap(Ty, NumTy),

    // Math
    AddInt(Reg<Int>, Reg<Int>, Reg<Int>),
    AddFloat(Reg<Float>, Reg<Float>, Reg<Float>),
    MulFloat(Reg<Float>, Reg<Float>, Reg<Float>),
    MulInt(Reg<Int>, Reg<Int>, Reg<Int>),
    Div(Reg<Float>, Reg<Float>, Reg<Float>),
    Pow(Reg<Float>, Reg<Float>, Reg<Float>),
    MinusFloat(Reg<Float>, Reg<Float>, Reg<Float>),
    MinusInt(Reg<Int>, Reg<Int>, Reg<Int>),
    ModFloat(Reg<Float>, Reg<Float>, Reg<Float>),
    ModInt(Reg<Int>, Reg<Int>, Reg<Int>),
    Not(Reg<Int>, Reg<Int>),
    NotStr(Reg<Int>, Reg<Str<'a>>),
    NegInt(Reg<Int>, Reg<Int>),
    NegFloat(Reg<Float>, Reg<Float>),
    Float1(FloatFunc, Reg<Float>, Reg<Float>),
    Float2(FloatFunc, Reg<Float>, Reg<Float>, Reg<Float>),
    Int1(Bitwise, Reg<Int>, Reg<Int>),
    Int2(Bitwise, Reg<Int>, Reg<Int>, Reg<Int>),
    Rand(Reg<Float>),
    Srand(
        /* previous seed */ Reg<Int>,
        /* new seed */ Reg<Int>,
    ),
    ReseedRng(/* previous seed */ Reg<Int>),

    // String processing
    Concat(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    StartsWithConst(Reg<Int>, Reg<Str<'a>>, Arc<[u8]>),
    IsMatch(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    IsMatchConst(Reg<Int>, Reg<Str<'a>>, Arc<Regex>),
    Match(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    MatchConst(Reg<Int>, Reg<Str<'a>>, Arc<Regex>),
    // index(s, t) returns index of substring t in s, 0 if it does not appear.
    SubstrIndex(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    LenStr(Reg<Int>, Reg<Str<'a>>),
    Sub(
        Reg<Int>,
        /*pat*/ Reg<Str<'a>>,
        /*for*/ Reg<Str<'a>>,
        /*in*/ Reg<Str<'a>>,
    ),
    GSub(
        Reg<Int>,
        /*pat*/ Reg<Str<'a>>,
        /*for*/ Reg<Str<'a>>,
        /*in*/ Reg<Str<'a>>,
    ),
    GenSubDynamic(
        Reg<Str<'a>>,
        /*pat*/ Reg<Str<'a>>,
        /*for*/ Reg<Str<'a>>,
        /*how*/ Reg<Str<'a>>,
        /*in*/ Reg<Str<'a>>,
    ),
    EscapeCSV(Reg<Str<'a>>, Reg<Str<'a>>),
    EscapeTSV(Reg<Str<'a>>, Reg<Str<'a>>),
    Substr(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Int>, Reg<Int>),

    // Comparison
    LTFloat(Reg<Int>, Reg<Float>, Reg<Float>),
    LTInt(Reg<Int>, Reg<Int>, Reg<Int>),
    LTStr(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    GTFloat(Reg<Int>, Reg<Float>, Reg<Float>),
    GTInt(Reg<Int>, Reg<Int>, Reg<Int>),
    GTStr(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    LTEFloat(Reg<Int>, Reg<Float>, Reg<Float>),
    LTEInt(Reg<Int>, Reg<Int>, Reg<Int>),
    LTEStr(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    GTEFloat(Reg<Int>, Reg<Float>, Reg<Float>),
    GTEInt(Reg<Int>, Reg<Int>, Reg<Int>),
    GTEStr(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    EQFloat(Reg<Int>, Reg<Float>, Reg<Float>),
    EQInt(Reg<Int>, Reg<Int>, Reg<Int>),
    EQStr(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),

    // Columns
    SetColumn(Reg<Int> /* dst column */, Reg<Str<'a>>),
    GetColumn(Reg<Str<'a>>, Reg<Int>),
    JoinCSV(
        Reg<Str<'a>>, /* dst */
        Reg<Int>,     /* start col */
        Reg<Int>,     /* end col */
    ),
    JoinTSV(
        Reg<Str<'a>>, /* dst */
        Reg<Int>,     /* start col */
        Reg<Int>,     /* end col */
    ),
    JoinColumns(
        Reg<Str<'a>>, /* dst */
        Reg<Int>,     /* start col */
        Reg<Int>,     /* end col */
        Reg<Str<'a>>, /* sep */
    ),
    ToUpperAscii(Reg<Str<'a>>, Reg<Str<'a>>),
    ToLowerAscii(Reg<Str<'a>>, Reg<Str<'a>>),

    // File reading.
    ReadErr(Reg<Int>, Reg<Str<'a>>, /*is_file=*/ bool),
    NextLine(Reg<Str<'a>>, Reg<Str<'a>>, /*is_file=*/ bool),
    ReadErrStdin(Reg<Int>),
    NextLineStdin(Reg<Str<'a>>),
    // Fetches line directly into $0.
    NextLineStdinFused(),
    // Advances early to the next file in our sequence
    NextFile(),
    Uuid(Reg<Str<'a>>, Reg<Str<'a>>),
    SnowFlake(Reg<Int>, Reg<Int>),
    Ulid(Reg<Str<'a>>),
    LocalIp(Reg<Str<'a>>),
    Whoami(Reg<Str<'a>>),
    Os(Reg<Str<'a>>),
    OsFamily(Reg<Str<'a>>),
    Arch(Reg<Str<'a>>),
    Pwd(Reg<Str<'a>>),
    UserHome(Reg<Str<'a>>),
    Strftime(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Int>),
    Encode(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Decode(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Digest(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Hmac(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Jwt(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>, Reg<runtime::StrMap<'a, Str<'a>>>),
    Dejwt( Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Mktime(Reg<Int>, Reg<Str<'a>>, Reg<Int>),
    MkBool(Reg<Int>, Reg<Str<'a>>),
    Systime(Reg<Int>),
    Fend(Reg<Str<'a>>, Reg<Str<'a>>),
    Min(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Max(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Seq(Reg<runtime::IntMap<Float>>, Reg<Float>, Reg<Float>, Reg<Float>),
    Url(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    SemVer(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    Path(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    DataUrl(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    DateTime(Reg<runtime::StrMap<'a, Int>>, Reg<Str<'a>>),
    Shlex(Reg<runtime::IntMap<Str<'a>>>, Reg<Str<'a>>),
    Uniq(Reg<runtime::IntMap<Str<'a>>>, Reg<runtime::IntMap<Str<'a>>>, Reg<Str<'a>>),
    TypeOfArray(Reg<Str<'a>>),
    TypeOfNumber(Reg<Str<'a>>),
    TypeOfString(Reg<Str<'a>>),
    TypeOfUnassigned(Reg<Str<'a>>),
    IsArrayTrue(Reg<Int>),
    IsArrayFalse(Reg<Int>),
    IsIntTrue(Reg<Int>),
    IsIntFalse(Reg<Int>),
    IsStrInt(Reg<Int>, Reg<Str<'a>>),
    IsNumTrue(Reg<Int>),
    IsNumFalse(Reg<Int>),
    IsStrNum(Reg<Int>, Reg<Str<'a>>),
    HttpGet(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>, Reg<runtime::StrMap<'a, Str<'a>>>),
    HttpPost(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>, Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    S3Get(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    S3Put(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    KvGet(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    KvPut(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    KvDelete(Reg<Str<'a>>, Reg<Str<'a>>),
    KvClear(Reg<Str<'a>>),
    SqliteQuery(Reg<runtime::IntMap<Str<'a>>>, Reg<Str<'a>>, Reg<Str<'a>>),
    SqliteExecute(Reg<Int>, Reg<Str<'a>>, Reg<Str<'a>>),
    Publish(Reg<Str<'a>>, Reg<Str<'a>>),
    FromJson(Reg<runtime::StrMap<'a, Str<'a>>>, Reg<Str<'a>>),
    MapIntIntToJson(Reg<Str<'a>>, Reg<runtime::IntMap<Int>>),
    MapIntFloatToJson(Reg<Str<'a>>, Reg<runtime::IntMap<Float>>),
    MapIntStrToJson(Reg<Str<'a>>, Reg<runtime::IntMap<Str<'a>>>),
    MapStrIntToJson(Reg<Str<'a>>, Reg<runtime::StrMap<'a, Int>>),
    MapStrFloatToJson(Reg<Str<'a>>, Reg<runtime::StrMap<'a, Float>>),
    MapStrStrToJson(Reg<Str<'a>>, Reg<runtime::StrMap<'a, Str<'a>>>),
    MapIntIntAsort(Reg<Int>, Reg<runtime::IntMap<Int>>, Reg<runtime::IntMap<Int>>),
    MapIntFloatAsort(Reg<Int>, Reg<runtime::IntMap<Float>>, Reg<runtime::IntMap<Float>>),
    MapIntStrAsort(Reg<Int>, Reg<runtime::IntMap<Str<'a>>>, Reg<runtime::IntMap<Str<'a>>>),
    MapIntIntJoin(Reg<Str<'a>>, Reg<runtime::IntMap<Int>>, Reg<Str<'a>>),
    MapIntFloatJoin(Reg<Str<'a>>, Reg<runtime::IntMap<Float>>, Reg<Str<'a>>),
    MapIntStrJoin(Reg<Str<'a>>, Reg<runtime::IntMap<Str<'a>>>, Reg<Str<'a>>),
    FromCsv(Reg<runtime::IntMap<Str<'a>>>, Reg<Str<'a>>),
    MapIntIntToCsv(Reg<Str<'a>>, Reg<runtime::IntMap<Int>>),
    MapIntFloatToCsv(Reg<Str<'a>>, Reg<runtime::IntMap<Float>>),
    MapIntStrToCsv(Reg<Str<'a>>, Reg<runtime::IntMap<Str<'a>>>),
    Trim(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Escape(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Str<'a>>),
    Truncate(Reg<Str<'a>>, Reg<Str<'a>>, Reg<Int>, Reg<Str<'a>>),
    Strtonum(Reg<Float>, Reg<Str<'a>>),
    Capitalize(Reg<Str<'a>>, Reg<Str<'a>>),
    Mask(Reg<Str<'a>>, Reg<Str<'a>>),
    UpdateUsedFields(),
    // Set the corresponding index in the FI variable. This is equivalent of loading FI, but we
    // keep this as a separate instruction to make static analysis easier.
    SetFI(Reg<Int>, Reg<Int>),

    // Split
    SplitInt(
        Reg<Int>,
        Reg<Str<'a>>,
        Reg<runtime::IntMap<Str<'a>>>,
        Reg<Str<'a>>,
    ),
    SplitStr(
        Reg<Int>,
        Reg<Str<'a>>,
        Reg<runtime::StrMap<'a, Str<'a>>>,
        Reg<Str<'a>>,
    ),
    Sprintf {
        dst: Reg<Str<'a>>,
        fmt: Reg<Str<'a>>,
        args: Vec<(NumTy, Ty)>,
    },
    Printf {
        output: Option<(Reg<Str<'a>>, FileSpec)>,
        fmt: Reg<Str<'a>>,
        args: Vec<(NumTy, Ty)>,
    },
    PrintAll {
        output: Option<(Reg<Str<'a>>, FileSpec)>,
        args: Vec<Reg<Str<'a>>>,
    },
    Close(Reg<Str<'a>>),
    RunCmd(Reg<Int>, Reg<Str<'a>>),
    Exit(Reg<Int>),

    // Map operations
    Lookup {
        map_ty: Ty,
        dst: NumTy,
        map: NumTy,
        key: NumTy,
    },
    Contains {
        map_ty: Ty,
        dst: NumTy,
        map: NumTy,
        key: NumTy,
    },
    Delete {
        map_ty: Ty,
        map: NumTy,
        key: NumTy,
    },
    Clear {
        map_ty: Ty,
        map: NumTy,
    },
    Len {
        map_ty: Ty,
        dst: NumTy,
        map: NumTy,
    },
    Store {
        map_ty: Ty,
        map: NumTy,
        key: NumTy,
        val: NumTy,
    },
    IncInt {
        map_ty: Ty,
        map: NumTy,
        key: NumTy,
        dst: NumTy,
        by: Reg<Int>,
    },
    IncFloat {
        map_ty: Ty,
        map: NumTy,
        key: NumTy,
        dst: NumTy,
        by: Reg<Float>,
    },
    IterBegin {
        map_ty: Ty,
        dst: NumTy,
        map: NumTy,
    },
    IterHasNext {
        iter_ty: Ty,
        dst: NumTy,
        iter: NumTy,
    },
    IterGetNext {
        iter_ty: Ty,
        dst: NumTy,
        iter: NumTy,
    },
    // Special variables
    LoadVarStr(Reg<Str<'a>>, Variable),
    StoreVarStr(Variable, Reg<Str<'a>>),
    LoadVarInt(Reg<Int>, Variable),
    StoreVarInt(Variable, Reg<Int>),
    LoadVarIntMap(Reg<runtime::IntMap<Str<'a>>>, Variable),
    StoreVarIntMap(Variable, Reg<runtime::IntMap<Str<'a>>>),
    LoadVarStrMap(Reg<runtime::StrMap<'a, Int>>, Variable),
    LoadVarStrStrMap(Reg<runtime::StrMap<'a, Str<'a>>>, Variable),
    StoreVarStrMap(Variable, Reg<runtime::StrMap<'a, Int>>),
    StoreVarStrStrMap(Variable, Reg<runtime::StrMap<'a, Str<'a>>>),

    LoadSlot {
        ty: Ty,
        slot: Int,
        dst: NumTy,
    },
    StoreSlot {
        ty: Ty,
        slot: Int,
        src: NumTy,
    },

    // Control
    JmpIf(Reg<Int>, Label),
    Jmp(Label),

    // Functions
    // TODO: we may need to push iterators as well?
    Push(Ty, NumTy),
    Pop(Ty, NumTy),
    Call(usize),
    Ret,
}

impl<T> Reg<T> {
    pub(crate) fn index(&self) -> usize {
        self.0 as usize
    }
}

// For accumulating register-specific metadata
pub(crate) trait Accum {
    fn reflect(&self) -> (NumTy, compile::Ty);
    fn accum(&self, mut f: impl FnMut(NumTy, compile::Ty)) {
        let (reg, ty) = self.reflect();
        f(reg, ty)
    }
}

pub(crate) trait Get<T> {
    fn get(&self, r: Reg<T>) -> &T;
    fn get_mut(&mut self, r: Reg<T>) -> &mut T;
}

fn _dbg_check_index<T>(desc: &str, Storage { regs, .. }: &Storage<T>, r: usize) {
    assert!(
        r < regs.len(),
        "[{}] index {} is out of bounds (len={})",
        desc,
        r,
        regs.len()
    );
}

macro_rules! impl_accum  {
    ($t:ty, $ty:tt, $($lt:tt),+) => {
        impl<$($lt),*> Accum for Reg<$t> {
            fn reflect(&self) -> (NumTy, compile::Ty) {
                (self.index() as NumTy, compile::Ty::$ty)
            }
        }
    };
    ($t:ty, $ty:tt,) => {
        impl Accum for Reg<$t> {
            fn reflect(&self) -> (NumTy, compile::Ty) {
                (self.index() as NumTy, compile::Ty::$ty)
            }
        }
    };
}

macro_rules! impl_get {
    ($t:ty, $fld:ident, $ty:tt $(,$lt:tt)*) => {
        impl_accum!($t, $ty, $($lt),*);
        impl<'a, LR: runtime::LineReader> Get<$t> for Interp<'a, LR> {
            #[inline(always)]
            fn get(&self, r: Reg<$t>) -> &$t {
                #[cfg(debug_assertions)]
                _dbg_check_index(
                    concat!(stringify!($t), "_", stringify!($fld)),
                    &self.$fld,
                    r.index(),
                );
                index(&self.$fld, &r)
            }
            #[inline(always)]
            fn get_mut(&mut self, r: Reg<$t>) -> &mut $t {
                #[cfg(debug_assertions)]
                _dbg_check_index(
                    concat!(stringify!($t), "_", stringify!($fld)),
                    &self.$fld,
                    r.index(),
                );
                index_mut(&mut self.$fld, &r)
            }
        }
    };
}

impl_get!(Int, ints, Int);
impl_get!(Str<'a>, strs, Str, 'a);
impl_get!(Float, floats, Float);
impl_get!(runtime::IntMap<Float>, maps_int_float, MapIntFloat);
impl_get!(runtime::IntMap<Int>, maps_int_int, MapIntInt);
impl_get!(runtime::IntMap<Str<'a>>, maps_int_str, MapIntStr, 'a);
impl_get!(runtime::StrMap<'a, Float>, maps_str_float, MapStrFloat, 'a);
impl_get!(runtime::StrMap<'a, Int>, maps_str_int, MapStrInt, 'a);
impl_get!(runtime::StrMap<'a, Str<'a>>, maps_str_str, MapStrStr, 'a);
impl_get!(runtime::Iter<Int>, iters_int, IterInt);
impl_get!(runtime::Iter<Str<'a>>, iters_str, IterStr, 'a);

// Helpful for avoiding big match statements when computing basic walks of the bytecode.
impl<'a> Instr<'a> {
    pub(crate) fn accum(&self, mut f: impl FnMut(NumTy, compile::Ty)) {
        use Instr::*;
        match self {
            StoreConstStr(sr, _s) => sr.accum(&mut f),
            StoreConstInt(ir, _i) => ir.accum(&mut f),
            StoreConstFloat(fr, _f) => fr.accum(&mut f),
            IntToStr(sr, ir) => {
                sr.accum(&mut f);
                ir.accum(&mut f)
            }
            FloatToStr(sr, fr) => {
                sr.accum(&mut f);
                fr.accum(&mut f);
            }
            Uuid(sr, version) => {
                sr.accum(&mut f);
                version.accum(&mut f);
            }
            SnowFlake(sr, machine_id) => {
                sr.accum(&mut f);
                machine_id.accum(&mut f);
            }
            Ulid(sr) => {
                sr.accum(&mut f);
            }
            Whoami(sr) | Os(sr) | OsFamily(sr)
            | Arch(sr) | Pwd(sr)| UserHome(sr)  => {
                sr.accum(&mut f);
            }
            LocalIp(sr) => {
                sr.accum(&mut f);
            }
            Systime(sr) => {
                sr.accum(&mut f);
            }
            Encode(res, format, text) => {
                res.accum(&mut f);
                format.accum(&mut f);
                text.accum(&mut f);
            }
            Decode(res, format, text) => {
                res.accum(&mut f);
                format.accum(&mut f);
                text.accum(&mut f);
            }
            Escape(res, format, text) => {
                res.accum(&mut f);
                format.accum(&mut f);
                text.accum(&mut f);
            }
            Digest(res, algorithm, text) => {
                res.accum(&mut f);
                algorithm.accum(&mut f);
                text.accum(&mut f);
            }
            Hmac(res, algorithm, key, text) => {
                res.accum(&mut f);
                algorithm.accum(&mut f);
                key.accum(&mut f);
                text.accum(&mut f);
            }
            Jwt(res, algorithm, key, payload) => {
                res.accum(&mut f);
                algorithm.accum(&mut f);
                key.accum(&mut f);
                payload.accum(&mut f);
            }
            Dejwt(res, key, token) => {
                res.accum(&mut f);
                key.accum(&mut f);
                token.accum(&mut f);
            }
            Strftime(res, format, timestamp) => {
                res.accum(&mut f);
                format.accum(&mut f);
                timestamp.accum(&mut f);
            }
            Mktime(res, date_time_text,timezone) => {
                res.accum(&mut f);
                date_time_text.accum(&mut f);
                timezone.accum(&mut f);
            }
            MkBool(res, text) => {
                res.accum(&mut f);
                text.accum(&mut f);
            }
            Fend(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            Url(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            SemVer(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            Path(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            DataUrl(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            DateTime(dst, timestamp) => {
                dst.accum(&mut f);
                timestamp.accum(&mut f);
            }
            Shlex(dst, text) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            HttpGet(dst, url,headers) => {
                dst.accum(&mut f);
                url.accum(&mut f);
                headers.accum(&mut f);
            }
            HttpPost(dst, url,headers, body) => {
                dst.accum(&mut f);
                url.accum(&mut f);
                headers.accum(&mut f);
                body.accum(&mut f);
            }
            S3Get(dst, bucket,object_name) => {
                dst.accum(&mut f);
                bucket.accum(&mut f);
                object_name.accum(&mut f);
            }
            S3Put(dst, bucket,object_name, body) => {
                dst.accum(&mut f);
                bucket.accum(&mut f);
                object_name.accum(&mut f);
                body.accum(&mut f);
            }
            KvGet(dst, namespace, key) => {
                dst.accum(&mut f);
                namespace.accum(&mut f);
                key.accum(&mut f);
            }
            KvPut(namespace, key,value) => {
                namespace.accum(&mut f);
                key.accum(&mut f);
                value.accum(&mut f);
            }
            KvDelete(namespace, key) => {
                namespace.accum(&mut f);
                key.accum(&mut f);
            }
            KvClear( namespace) => {
                namespace.accum(&mut f);
            }
            SqliteQuery(dst, db_path, sql) => {
                dst.accum(&mut f);
                db_path.accum(&mut f);
                sql.accum(&mut f);
            }
            SqliteExecute(dst, db_path, sql) => {
                dst.accum(&mut f);
                db_path.accum(&mut f);
                sql.accum(&mut f);
            }
            Publish(namespace, body) => {
                namespace.accum(&mut f);
                body.accum(&mut f);
            }
            FromJson(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            MapIntIntToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapIntFloatToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapIntStrToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapStrIntToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapStrFloatToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapStrStrToJson(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapIntIntAsort( dst, arr, target) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                target.accum(&mut f);
            }
            MapIntFloatAsort(dst, arr,target) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                target.accum(&mut f);
            }
            MapIntStrAsort(dst, arr,target) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                target.accum(&mut f);
            }
            MapIntIntJoin( dst, arr, target) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                target.accum(&mut f);
            }
            MapIntFloatJoin(dst, arr,sep) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                sep.accum(&mut f);
            }
            MapIntStrJoin(dst, arr,sep) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
                sep.accum(&mut f);
            }
            FromCsv(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            MapIntIntToCsv(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapIntFloatToCsv(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            MapIntStrToCsv(dst, arr) => {
                dst.accum(&mut f);
                arr.accum(&mut f);
            }
            Max(dst, first, second,third) => {
                dst.accum(&mut f);
                first.accum(&mut f);
                second.accum(&mut f);
                third.accum(&mut f);
            }
            Min(dst, first, second,third) => {
                dst.accum(&mut f);
                first.accum(&mut f);
                second.accum(&mut f);
                third.accum(&mut f);
            }
            Seq(dst, start, step,end) => {
                dst.accum(&mut f);
                start.accum(&mut f);
                step.accum(&mut f);
                end.accum(&mut f);
            }
            Uniq(dst, src, param) => {
                dst.accum(&mut f);
                src.accum(&mut f);
                param.accum(&mut f);
            }
            Trim(dst, src, pat ) => {
                dst.accum(&mut f);
                src.accum(&mut f);
                pat.accum(&mut f);
            }
            Truncate(dst, src, len, place_holder ) => {
                dst.accum(&mut f);
                src.accum(&mut f);
                len.accum(&mut f);
                place_holder.accum(&mut f);
            }
            Strtonum(dst, text ) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            Capitalize(dst, text ) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            Mask(dst, text ) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            TypeOfArray(_dst) => {
            }
            TypeOfNumber(_dst) => {
            }
            TypeOfString(_dst) => {
            }
            TypeOfUnassigned(_dst) => {
            }
            IsArrayTrue(dst ) => {
                dst.accum(&mut f);
            }
            IsArrayFalse(dst ) => {
                dst.accum(&mut f);
            }
            IsIntTrue(dst ) => {
                dst.accum(&mut f);
            }
            IsIntFalse(dst ) => {
                dst.accum(&mut f);
            }
            IsStrInt(dst , text) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            IsNumTrue(dst ) => {
                dst.accum(&mut f);
            }
            IsNumFalse(dst ) => {
                dst.accum(&mut f);
            }
            IsStrNum(dst , text) => {
                dst.accum(&mut f);
                text.accum(&mut f);
            }
            StrToInt(ir, sr) | HexStrToInt(ir, sr) => {
                ir.accum(&mut f);
                sr.accum(&mut f);
            }
            StrToFloat(fr, sr) => {
                fr.accum(&mut f);
                sr.accum(&mut f);
            }
            FloatToInt(ir, fr) => {
                ir.accum(&mut f);
                fr.accum(&mut f);
            }
            IntToFloat(fr, ir) => {
                fr.accum(&mut f);
                ir.accum(&mut f);
            }
            AddInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            AddFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            MulInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            MulFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            MinusInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            MinusFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            ModInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            ModFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            Pow(res, l, r) | Div(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            Not(res, ir) => {
                res.accum(&mut f);
                ir.accum(&mut f)
            }
            NotStr(res, sr) => {
                res.accum(&mut f);
                sr.accum(&mut f)
            }
            NegInt(res, ir) => {
                res.accum(&mut f);
                ir.accum(&mut f)
            }
            NegFloat(res, fr) => {
                res.accum(&mut f);
                fr.accum(&mut f)
            }
            Float1(_, dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            Float2(_, dst, x, y) => {
                dst.accum(&mut f);
                x.accum(&mut f);
                y.accum(&mut f);
            }
            Int1(_, dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            Int2(_, dst, x, y) => {
                dst.accum(&mut f);
                x.accum(&mut f);
                y.accum(&mut f);
            }
            Rand(res) => res.accum(&mut f),
            Srand(res, seed) => {
                res.accum(&mut f);
                seed.accum(&mut f)
            }
            ReseedRng(res) => res.accum(&mut f),
            StartsWithConst(res, s, _) => {
                res.accum(&mut f);
                s.accum(&mut f);
            }
            Concat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            Match(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            IsMatch(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            MatchConst(res, src, _) | IsMatchConst(res, src, _) => {
                res.accum(&mut f);
                src.accum(&mut f);
            }
            SubstrIndex(res, s, t) => {
                res.accum(&mut f);
                s.accum(&mut f);
                t.accum(&mut f);
            }
            LenStr(res, s) => {
                res.accum(&mut f);
                s.accum(&mut f)
            }
            GSub(res, pat, s, in_s) | Sub(res, pat, s, in_s) => {
                res.accum(&mut f);
                pat.accum(&mut f);
                s.accum(&mut f);
                in_s.accum(&mut f);
            }
            GenSubDynamic(res, pat, s, how, in_s) => {
                res.accum(&mut f);
                pat.accum(&mut f);
                s.accum(&mut f);
                how.accum(&mut f);
                in_s.accum(&mut f);
            }
            EscapeCSV(res, s) | EscapeTSV(res, s) => {
                res.accum(&mut f);
                s.accum(&mut f);
            }
            Substr(res, base, l, r) => {
                res.accum(&mut f);
                base.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTStr(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTStr(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTEFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTEInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            LTEStr(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTEFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTEInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            GTEStr(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            EQFloat(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            EQInt(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            EQStr(res, l, r) => {
                res.accum(&mut f);
                l.accum(&mut f);
                r.accum(&mut f);
            }
            SetColumn(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f)
            }
            GetColumn(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f)
            }
            JoinCSV(dst, start, end) | JoinTSV(dst, start, end) => {
                dst.accum(&mut f);
                start.accum(&mut f);
                end.accum(&mut f);
            }
            JoinColumns(dst, start, end, sep) => {
                dst.accum(&mut f);
                start.accum(&mut f);
                end.accum(&mut f);
                sep.accum(&mut f);
            }
            ToUpperAscii(dst, src) | ToLowerAscii(dst, src) => {
                dst.accum(&mut f);
                src.accum(&mut f);
            }
            SplitInt(flds, to_split, arr, pat) => {
                flds.accum(&mut f);
                to_split.accum(&mut f);
                arr.accum(&mut f);
                pat.accum(&mut f);
            }
            SplitStr(flds, to_split, arr, pat) => {
                flds.accum(&mut f);
                to_split.accum(&mut f);
                arr.accum(&mut f);
                pat.accum(&mut f);
            }
            Sprintf { dst, fmt, args } => {
                dst.accum(&mut f);
                fmt.accum(&mut f);
                for (reg, ty) in args.iter().cloned() {
                    f(reg, ty);
                }
            }
            Printf { output, fmt, args } => {
                if let Some((path_reg, _)) = output {
                    path_reg.accum(&mut f);
                }
                fmt.accum(&mut f);
                for (reg, ty) in args.iter().cloned() {
                    f(reg, ty);
                }
            }
            PrintAll { output, args } => {
                if let Some((path_reg, _)) = output {
                    path_reg.accum(&mut f);
                }
                for reg in args {
                    reg.accum(&mut f)
                }
            }
            Close(file) => file.accum(&mut f),
            RunCmd(dst, cmd) => {
                dst.accum(&mut f);
                cmd.accum(&mut f);
            }
            Exit(code) => code.accum(&mut f),
            Lookup {
                map_ty,
                dst,
                map,
                key,
            } => {
                let (k, v) = (map_ty.key().unwrap(), map_ty.val().unwrap());
                f(*dst, v);
                f(*key, k);
                f(*map, *map_ty);
            }
            Contains {
                map_ty,
                dst,
                map,
                key,
            } => {
                let k = map_ty.key().unwrap();
                f(*dst, Ty::Int);
                f(*key, k);
                f(*map, *map_ty);
            }
            Delete { map_ty, map, key } => {
                let k = map_ty.key().unwrap();
                f(*key, k);
                f(*map, *map_ty);
            }
            Clear { map_ty, map } => f(*map, *map_ty),
            Len { map_ty, map, dst } => {
                f(*dst, Ty::Int);
                f(*map, *map_ty);
            }
            IterBegin { map_ty, map, dst } => {
                f(*dst, map_ty.key_iter().unwrap());
                f(*map, *map_ty);
            }
            Store {
                map_ty,
                map,
                key,
                val,
            } => {
                f(*map, *map_ty);
                f(*key, map_ty.key().unwrap());
                f(*val, map_ty.val().unwrap());
            }
            IncInt {
                map_ty,
                map,
                key,
                dst,
                by,
            } => {
                f(*map, *map_ty);
                f(*key, map_ty.key().unwrap());
                f(*dst, map_ty.val().unwrap());
                by.accum(&mut f);
            }
            IncFloat {
                map_ty,
                map,
                key,
                dst,
                by,
            } => {
                f(*map, *map_ty);
                f(*key, map_ty.key().unwrap());
                f(*dst, map_ty.val().unwrap());
                by.accum(&mut f);
            }
            LoadVarStr(dst, _var) => dst.accum(&mut f),
            StoreVarStr(_var, src) => src.accum(&mut f),
            LoadVarInt(dst, _var) => dst.accum(&mut f),
            StoreVarInt(_var, src) => src.accum(&mut f),
            LoadVarIntMap(dst, _var) => dst.accum(&mut f),
            StoreVarIntMap(_var, src) => src.accum(&mut f),
            LoadVarStrMap(dst, _var) => dst.accum(&mut f),
            StoreVarStrMap(_var, src) => src.accum(&mut f),
            LoadVarStrStrMap(dst, _var) => dst.accum(&mut f),
            StoreVarStrStrMap(_var, src) => src.accum(&mut f),

            LoadSlot { ty, dst, .. } => f(*dst, *ty),
            StoreSlot { ty, src, .. } => f(*src, *ty),

            IterHasNext { iter_ty, dst, iter } => {
                f(*dst, Ty::Int);
                f(*iter, *iter_ty);
            }
            IterGetNext { iter_ty, dst, iter } => {
                f(*dst, iter_ty.iter().unwrap());
                f(*iter, *iter_ty);
            }
            Mov(ty, dst, src) => {
                f(*dst, *ty);
                f(*src, *ty);
            }
            AllocMap(ty, reg) => f(*reg, *ty),
            ReadErr(dst, file, _) => {
                dst.accum(&mut f);
                file.accum(&mut f)
            }
            NextLine(dst, file, _) => {
                dst.accum(&mut f);
                file.accum(&mut f)
            }
            ReadErrStdin(dst) => dst.accum(&mut f),
            NextLineStdin(dst) => dst.accum(&mut f),
            JmpIf(cond, _lbl) => cond.accum(&mut f),
            Push(ty, reg) => f(*reg, *ty),
            Pop(ty, reg) => f(*reg, *ty),
            SetFI(key, val) => {
                key.accum(&mut f);
                val.accum(&mut f);
            }
            UpdateUsedFields() | NextFile() | NextLineStdinFused() | Call(_) | Jmp(_) | Ret => {}
        }
    }
}
