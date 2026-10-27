//! This module contains definitions and metadata for builtin functions and builtin variables.
use crate::ast;
#[allow(unused_imports)]
use crate::common::Either;
use crate::common::{CompileError, NodeIx, Result};
use crate::compile;
use crate::runtime::{Int, IntMap, Str, StrMap};
use crate::types::{self, SmallVec};
use smallvec::smallvec;

use std::convert::TryFrom;

pub const VERSION: &'static str = env!("CARGO_PKG_VERSION");

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Function {
    // Internal: the identity on strings, marking its result as a "strnum" (a string from input,
    // which compares numerically if it looks like a number). Used for -v assignments.
    Strnum,
    // Internal: converts a number to a float. Sums and differences of two non-constant integers
    // are computed in floating point (awk numbers are doubles), so they cannot overflow.
    ToFloat,
    // Internal: converts a value to a string the way print does, formatting floats with OFMT.
    // Used for print arguments that may be uninitialized (see `uninit`).
    OfmtStr,
    Unop(ast::Unop),
    Binop(ast::Binop),
    FloatFunc(FloatFunc),
    IntFunc(Bitwise),
    Close,
    Fflush,
    ReadErr,
    ReadErrCmd,
    Nextline,
    ReadErrStdin,
    NextlineStdin,
    NextlineCmd,
    ReadLineStdinFused,
    NextFile,
    Setcol,
    Split,
    // split(s, arr, fs, seps)
    SplitSeps,
    // patsplit(s, arr, re, seps)
    PatSplit,
    Length,
    Strlen,
    Uuid,
    Ulid,
    Tsid,
    SnowFlake,
    Whoami,
    Version,
    Os,
    OsFamily,
    Arch,
    Pwd,
    UserHome,
    GetEnv,
    LogDebug,
    LogInfo,
    LogWarn,
    LogError,
    Systime,
    Strftime,
    Mktime,
    Duration,
    MkBool,
    MkPassword,
    Fend,
    Eval,
    Trim,
    Truncate,
    Parse,
    RegexParse,
    // zawk's float(): decimal, or hexadecimal, octal and binary with a 0x, 0o or 0b prefix.
    Strtonum,
    // gawk's strtonum(): a leading 0 or 0x makes the string octal or hexadecimal.
    AwkStrtonum,
    FormatBytes,
    ToBytes,
    StartsWith,
    EndsWith,
    Capitalize,
    TextContains,
    UnCapitalize,
    CamelCase,
    KebabCase,
    SnakeCase,
    TitleCase,
    PascalCase,
    Figlet,
    PadLeft,
    PadRight,
    PadBoth,
    StrCmp,
    Similar,
    Mask,
    Repeat,
    Words,
    Lines,
    DefaultIfEmpty,
    AppendIfMissing,
    PrependIfMissing,
    RemoveIfEnd,
    RemoveIfBegin,
    Quote,
    DoubleQuote,
    Escape,
    Encode,
    Decode,
    Digest,
    Hmac,
    Jwt,
    Dejwt,
    Encrypt,
    Decrypt,
    Url,
    Pairs,
    Record,
    Message,
    Flags,
    SemVer,
    Path,
    DataUrl,
    DateTime,
    Shlex,
    Func,
    Tuple,
    Variant,
    ParseArray,
    Hex2Rgb,
    Rgb2Hex,
    FromJson,
    ToJson,
    JsonValue,
    JsonQuery,
    JsonHas,
    JsonGet,
    JsonSet,
    JsonDel,
    HtmlValue,
    HtmlQuery,
    XmlValue,
    XmlQuery,
    VarDump,
    ReadAll,
    WriteAll,
    ReadConfig,
    FromCsv,
    ToCsv,
    HttpGet,
    HttpPost,
    SendMail,
    SmtpSend,
    S3Get,
    S3Put,
    KvGet,
    KvPut,
    KvDelete,
    KvClear,
    SqliteQuery,
    SqliteExecute,
    MysqlQuery,
    MysqlExecute,
    PgQuery,
    PgExecute,
    Publish,
    Min,
    Max,
    Seq,
    ArrayMax,
    ArrayMin,
    ArrayMean,
    ArraySum,
    // _median(a), _quantile(a, q), _var(a), _stddev(a), _mode(a): rewritten to
    // ArrayStat(a, kind, q) when lowering to the CFG (see runtime::array_stats).
    ArrayMedian,
    ArrayQuantile,
    ArrayVar,
    ArrayStddev,
    ArrayMode,
    ArrayStat,
    ArrayCountDistinct,
    ArrayTop,
    Asort,
    Asorti,
    BloomFilterInsert,
    BloomFilterContains,
    BloomFilterContainsWithInsert,
    Fake,
    Jev,
    JevProb,
    JevChoice,
    JevScore,
    LocalIp,
    Contains,
    Delete,
    Clear,
    Match,
    // gawk's match(s, re, arr); `match` with three arguments maps to it.
    MatchArr,
    SubstrIndex,
    SubstrLastIndex,
    LastPart,
    Sub,
    GSub,
    GenSub,
    EscapeCSV,
    EscapeTSV,
    JoinCols,
    JoinCSV,
    JoinTSV,
    IntMapJoin,
    Uniq,
    TypeOfVariable,
    // typeof() of a variable that is never read: "untyped" rather than "unassigned" when it is
    // also never assigned (see the `uninit` module).
    TypeOfUntyped,
    IsArray,
    IsInt,
    IsNum,
    IsFormat,
    Substr,
    CharAt,
    Chars,
    ToInt,
    HexToInt,
    Rand,
    Srand,
    ReseedRng,
    System,
    System2,
    // For header-parsing logic
    UpdateUsedFields,
    SetFI,
    ToUpper,
    ToLower,
    IncMap,
    Exit,
    // `exit` in a parallel program: record the exit code (unless the second argument is non-zero)
    // and stop reading input; the caller then returns.
    SetExit,
    // Whether `exit` was called (in a parallel program), checked after calls to user-defined
    // functions.
    Exiting,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Bitwise {
    Complement,
    And,
    Or,
    LogicalRightShift,
    ArithmeticRightShift,
    LeftShift,
    Xor,
}

/// Maps a shift count to `u32`, sending negative counts out of range (≥ 64).
fn shift_count(n: i64) -> u32 {
    u32::try_from(n as u64).unwrap_or(u32::MAX)
}

impl Bitwise {
    pub fn func_name(&self) -> &'static str {
        use Bitwise::*;
        match self {
            Complement => "compl",
            And => "and",
            Or => "or",
            LogicalRightShift => "rshiftl",
            ArithmeticRightShift => "rshift",
            LeftShift => "lshift",
            Xor => "xor",
        }
    }
    pub fn eval1(&self, op: i64) -> i64 {
        use Bitwise::*;
        match self {
            Complement => !op,
            And | Or | LogicalRightShift | ArithmeticRightShift | LeftShift | Xor => {
                panic!("bitwise: mismatched arity!")
            }
        }
    }
    pub fn eval2(&self, lhs: i64, rhs: i64) -> i64 {
        use Bitwise::*;
        match self {
            And => lhs & rhs,
            Or => lhs | rhs,
            // Shift counts outside 0..64 (including negative ones) shift every bit out.
            LogicalRightShift => (lhs as u64).checked_shr(shift_count(rhs)).unwrap_or(0) as i64,
            ArithmeticRightShift => lhs >> shift_count(rhs).min(63),
            LeftShift => lhs.checked_shl(shift_count(rhs)).unwrap_or(0),
            Xor => lhs ^ rhs,
            Complement => panic!("bitwise: mismatched arity!"),
        }
    }
    pub fn arity(&self) -> usize {
        use Bitwise::*;
        match self {
            Complement => 1,
            And | Or | LogicalRightShift | ArithmeticRightShift | LeftShift | Xor => 2,
        }
    }
    fn sig(&self) -> (SmallVec<compile::Ty>, compile::Ty) {
        use compile::Ty;
        (smallvec![Ty::Int; self.arity()], Ty::Int)
    }
    fn ret_state(&self) -> types::State {
        types::TVar::Scalar(types::BaseTy::Int).abs()
    }
}

/// Integer division truncated toward zero, returning 0 instead of failing when `y` is 0 (or the
/// quotient is not finite).
pub fn intdiv0(x: f64, y: f64) -> f64 {
    let q = (x.trunc() / y.trunc()).trunc();
    if q.is_finite() { q } else { 0.0 }
}

// TODO: move the llvm-level code back into the LLVM module.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FloatFunc {
    Cos,
    Sin,
    Atan,
    Atan2,
    // Integer division truncated toward zero; 0 when the divisor is 0.
    IntDiv0,
    // Natural log
    Log,
    // Log base 2
    Log2,
    // Log base 10
    Log10,
    Sqrt,
    // e^
    Exp,
    Abs,
    Ceil,
    Floor,
    Round,
    // Truncation toward zero: `int(x)` for a float or string `x`. Not callable by name.
    Trunc,
}

impl FloatFunc {
    pub fn eval1(&self, op: f64) -> f64 {
        use FloatFunc::*;
        match self {
            Cos => op.cos(),
            Sin => op.sin(),
            Atan => op.atan(),
            Log => op.ln(),
            Log2 => op.log2(),
            Log10 => op.log10(),
            Sqrt => op.sqrt(),
            Exp => op.exp(),
            Abs => op.abs(),
            Ceil => op.ceil(),
            Floor => op.floor(),
            Round => op.round(),
            Trunc => op.trunc(),
            Atan2 | IntDiv0 => panic!("float: mismatched arity!"),
        }
    }
    pub fn eval2(&self, x: f64, y: f64) -> f64 {
        use FloatFunc::*;
        match self {
            Atan2 => x.atan2(y),
            IntDiv0 => intdiv0(x, y),
            Sqrt | Cos | Sin | Atan | Log | Log2 | Log10 | Exp | Abs | Ceil | Floor | Round | Trunc => {
                panic!("float: mismatched arity!")
            }
        }
    }

    pub fn func_name(&self) -> &'static str {
        use FloatFunc::*;
        match self {
            Cos => "cos",
            Sin => "sin",
            Atan => "atan",
            Log => "log",
            Log2 => "log2",
            Log10 => "log10",
            Sqrt => "sqrt",
            Atan2 => "atan2",
            IntDiv0 => "intdiv0",
            Exp => "exp",
            Abs => "abs",
            Ceil => "ceil",
            Floor => "floor",
            Round => "round",
            Trunc => "int",
        }
    }

    pub fn arity(&self) -> usize {
        use FloatFunc::*;
        match self {
            Sqrt | Cos | Sin | Atan | Log | Log2 | Log10 | Exp | Abs | Ceil | Floor | Round | Trunc => 1,
            Atan2 | IntDiv0 => 2,
        }
    }
    fn sig(&self) -> (SmallVec<compile::Ty>, compile::Ty) {
        use compile::Ty;
        (smallvec![Ty::Float; self.arity()], Ty::Float)
    }
    fn ret_state(&self) -> types::State {
        types::TVar::Scalar(types::BaseTy::Float).abs()
    }
}

// This map is used to look up functions that are called in the program source and determine if
// they are builtin functions. Note that not all members of the Function enum are present here.
// This includes only the "public" functions.
static_map!(
    FUNCTIONS<&'static str, Function>,
    ["close", Function::Close],
    ["fflush", Function::Fflush],
    ["split", Function::Split],
    ["patsplit", Function::PatSplit],
    ["strlen", Function::Strlen],
    ["length", Function::Length],
    ["uuid", Function::Uuid],
    ["ulid", Function::Ulid],
    ["tsid", Function::Tsid],
    ["snowflake", Function::SnowFlake],
    ["whoami", Function::Whoami],
    ["version", Function::Version],
    ["os", Function::Os],
    ["os_family", Function::OsFamily],
    ["arch", Function::Arch],
    ["pwd", Function::Pwd],
    ["user_home", Function::UserHome],
    ["getenv", Function::GetEnv],
    ["log_debug", Function::LogDebug],
    ["log_info", Function::LogInfo],
    ["log_warn", Function::LogWarn],
    ["log_error", Function::LogError],
    ["systime", Function::Systime],
    ["strftime", Function::Strftime],
    ["mktime", Function::Mktime],
    ["duration", Function::Duration],
    ["mkbool", Function::MkBool],
    ["mkpass", Function::MkPassword],
    ["fend", Function::Fend],
    ["eval", Function::Eval],
    ["trim", Function::Trim],
    ["encode", Function::Encode],
    ["decode", Function::Decode],
    ["digest", Function::Digest],
    ["hash", Function::Digest],
    ["hmac", Function::Hmac],
    ["jwt", Function::Jwt],
    ["dejwt", Function::Dejwt],
    ["encrypt", Function::Encrypt],
    ["decrypt", Function::Decrypt],
    ["data_url", Function::DataUrl],
    ["url", Function::Url],
    ["pairs", Function::Pairs],
    ["record", Function::Record],
    ["message", Function::Message],
    ["flags", Function::Flags],
    ["semver", Function::SemVer],
    ["path", Function::Path],
    ["datetime", Function::DateTime],
    ["shlex", Function::Shlex],
    ["tuple", Function::Tuple],
    ["variant", Function::Variant],
    ["parse_array", Function::ParseArray],
    ["hex2rgb", Function::Hex2Rgb],
    ["rgb2hex", Function::Rgb2Hex],
    ["func", Function::Func],
    ["http_get", Function::HttpGet],
    ["http_post", Function::HttpPost],
    ["send_mail", Function::SendMail],
    ["send_email", Function::SendMail],
    ["smtp_send", Function::SmtpSend],
    ["s3_get", Function::S3Get],
    ["s3_put", Function::S3Put],
    ["kv_get", Function::KvGet],
    ["kv_put", Function::KvPut],
    ["kv_delete", Function::KvDelete],
    ["kv_clear", Function::KvClear],
    ["sqlite_query", Function::SqliteQuery],
    ["sqlite_execute", Function::SqliteExecute],
    ["mysql_query", Function::MysqlQuery],
    ["mysql_execute", Function::MysqlExecute],
    ["pg_query", Function::PgQuery],
    ["pg_execute", Function::PgExecute],
    ["publish", Function::Publish],
    ["from_json", Function::FromJson],
    ["to_json", Function::ToJson],
    ["json_value", Function::JsonValue],
    ["json_query", Function::JsonQuery],
    ["json_has", Function::JsonHas],
    ["json_get", Function::JsonGet],
    ["json_set", Function::JsonSet],
    ["json_del", Function::JsonDel],
    ["html_value", Function::HtmlValue],
    ["html_query", Function::HtmlQuery],
    ["xml_value", Function::XmlValue],
    ["xml_query", Function::XmlQuery],
    ["var_dump", Function::VarDump],
    ["read_all", Function::ReadAll],
    ["write_all", Function::WriteAll],
    ["read_config", Function::ReadConfig],
    ["pprint", Function::VarDump],
    ["from_csv", Function::FromCsv],
    ["to_csv", Function::ToCsv],
    ["min", Function::Min],
    ["max", Function::Max],
    // array underscore functions
    ["_max", Function::ArrayMax],
    ["_min", Function::ArrayMin],
    ["_sum", Function::ArraySum],
    ["_mean", Function::ArrayMean],
    ["_median", Function::ArrayMedian],
    ["_quantile", Function::ArrayQuantile],
    ["_var", Function::ArrayVar],
    ["_stddev", Function::ArrayStddev],
    ["_mode", Function::ArrayMode],
    ["_count_distinct", Function::ArrayCountDistinct],
    ["_top", Function::ArrayTop],
    ["_join", Function::IntMapJoin],
    ["seq", Function::Seq],
    ["uniq", Function::Uniq],
    ["asort", Function::Asort],
    ["asorti", Function::Asorti],
    ["bf_insert", Function::BloomFilterInsert],
    ["bf_contains", Function::BloomFilterContains],
    ["bf_icontains", Function::BloomFilterContainsWithInsert],
    ["fake", Function::Fake],
    ["jev", Function::Jev],
    ["jev_prob", Function::JevProb],
    ["jev_choice", Function::JevChoice],
    ["jev_score", Function::JevScore],
    ["local_ip", Function::LocalIp],
    ["truncate", Function::Truncate],
    ["parse", Function::Parse],
    ["rparse", Function::RegexParse],
    ["strtonum", Function::AwkStrtonum],
    ["format_bytes", Function::FormatBytes],
    ["to_bytes", Function::ToBytes],
    ["starts_with", Function::StartsWith],
    ["ends_with", Function::EndsWith],
    ["contains", Function::TextContains],
    ["capitalize", Function::Capitalize],
    ["uncapitalize", Function::UnCapitalize],
    ["camel_case", Function::CamelCase],
    ["kebab_case", Function::KebabCase],
    ["snake_case", Function::SnakeCase],
    ["title_case", Function::TitleCase],
    ["pascal_case", Function::PascalCase],
    ["figlet", Function::Figlet],
    ["pad_end", Function::PadLeft],
    ["pad_start", Function::PadRight],
    ["pad", Function::PadBoth],
    ["strcmp", Function::StrCmp],
    ["similar", Function::Similar],
    ["mask", Function::Mask],
    ["repeat", Function::Repeat],
    ["default_if_empty", Function::DefaultIfEmpty],
    ["append_if_missing", Function::AppendIfMissing],
    ["prepend_if_missing", Function::PrependIfMissing],
    ["remove_if_end", Function::RemoveIfEnd],
    ["remove_if_begin", Function::RemoveIfBegin],
    ["quote", Function::Quote],
    ["double_quote", Function::DoubleQuote],
    ["words", Function::Words],
    ["lines", Function::Lines],
    ["escape", Function::Escape],
    ["typeof", Function::TypeOfVariable],
    ["isarray", Function::IsArray],
    ["isint", Function::IsInt],
    ["isnum", Function::IsNum],
    ["is", Function::IsFormat],
    ["match", Function::Match],
    ["sub", Function::Sub],
    ["gsub", Function::GSub],
    ["gensub", Function::GenSub],
    ["substr", Function::Substr],
    ["char_at", Function::CharAt],
    ["chars", Function::Chars],
    ["int", Function::ToInt],
    ["float", Function::Strtonum],
    ["hex", Function::HexToInt],
    ["exp", Function::FloatFunc(FloatFunc::Exp)],
    ["abs", Function::FloatFunc(FloatFunc::Abs)],
    ["ceil", Function::FloatFunc(FloatFunc::Ceil)],
    ["floor", Function::FloatFunc(FloatFunc::Floor)],
    ["round", Function::FloatFunc(FloatFunc::Round)],
    ["cos", Function::FloatFunc(FloatFunc::Cos)],
    ["sin", Function::FloatFunc(FloatFunc::Sin)],
    ["atan", Function::FloatFunc(FloatFunc::Atan)],
    ["log", Function::FloatFunc(FloatFunc::Log)],
    ["log2", Function::FloatFunc(FloatFunc::Log2)],
    ["log10", Function::FloatFunc(FloatFunc::Log10)],
    ["sqrt", Function::FloatFunc(FloatFunc::Sqrt)],
    ["atan2", Function::FloatFunc(FloatFunc::Atan2)],
    ["intdiv0", Function::FloatFunc(FloatFunc::IntDiv0)],
    ["and", Function::IntFunc(Bitwise::And)],
    ["or", Function::IntFunc(Bitwise::Or)],
    ["compl", Function::IntFunc(Bitwise::Complement)],
    ["lshift", Function::IntFunc(Bitwise::LeftShift)],
    ["rshift", Function::IntFunc(Bitwise::ArithmeticRightShift)],
    ["rshiftl", Function::IntFunc(Bitwise::LogicalRightShift)],
    ["xor", Function::IntFunc(Bitwise::Xor)],
    ["join_fields", Function::JoinCols],
    ["join_csv", Function::JoinCSV],
    ["join_tsv", Function::JoinTSV],
    ["escape_csv", Function::EscapeCSV],
    ["escape_tsv", Function::EscapeTSV],
    ["rand", Function::Rand],
    ["srand", Function::Srand],
    ["index", Function::SubstrIndex],
    ["last_index", Function::SubstrLastIndex],
    ["last_part", Function::LastPart],
    ["toupper", Function::ToUpper],
    ["tolower", Function::ToLower],
    ["system", Function::System],
    ["system2", Function::System2],
    ["exit", Function::Exit]
);

impl<'a> TryFrom<&'a str> for Function {
    type Error = ();
    // error means not found
    fn try_from(value: &'a str) -> std::result::Result<Function, ()> {
        match FUNCTIONS.get(value) {
            Some(v) => Ok(*v),
            None => Err(()),
        }
    }
}

impl Function {
    /// The name of the function in awk programs, if it has one (used in error messages).
    pub(crate) fn awk_name(&self) -> Option<&'static str> {
        FUNCTIONS.iter().filter(|(_, f)| *f == self).map(|(name, _)| *name).min()
    }
}

/// The error for a call of the builtin `name` with `got` arguments. The accepted counts come from
/// its documented signature (optional parameters are filled in with defaults), or are `arity`.
pub(crate) fn arity_error(name: &str, got: usize, arity: usize) -> CompileError {
    let (min, max) = crate::function_docs::arg_range(name).unwrap_or((arity, arity));
    let expected = match max - min {
        0 => min.to_string(),
        1 => format!("{} or {}", min, max),
        _ => format!("{} to {}", min, max),
    };
    let noun = if max == 1 { "argument" } else { "arguments" };
    err_raw!("function `{}` expects {} {}, got {}", name, expected, noun, got)
}

/// The builtin functions of POSIX awk and gawk that zawk implements. A user-defined function may
/// not have one of these names, as in gawk. The other builtins are zawk's stdlib extensions
/// (`trim`, `max`, `abs`, ...): a user-defined function with the same name takes precedence, so
/// that existing awk programs defining these helpers keep working.
pub(crate) fn is_awk_builtin(name: &str) -> bool {
    matches!(
        name,
        "length" | "substr" | "index" | "split" | "patsplit" | "sub" | "gsub" | "match" | "sprintf"
            | "sin" | "cos" | "atan2" | "exp" | "log" | "sqrt" | "int" | "rand" | "srand"
            | "tolower" | "toupper" | "system" | "close" | "fflush"
            // gawk
            | "gensub" | "strtonum" | "asort" | "asorti" | "typeof" | "isarray" | "systime" | "strftime"
            | "mktime" | "and" | "or" | "xor" | "compl" | "lshift" | "rshift"
    )
}

pub(crate) trait IsSprintf {
    fn is_sprintf(&self) -> bool;
}

impl<'a> IsSprintf for &'a str {
    fn is_sprintf(&self) -> bool {
        *self == "sprintf"
    }
}

/// Build identifiers of hidden variables generated by the compiler, such as the pending exit code.
pub(crate) trait FromStatic {
    fn from_static(name: &'static str) -> Self;
}

impl<'a> FromStatic for &'a str {
    fn from_static(name: &'static str) -> &'a str {
        name
    }
}

impl Function {
    // feedback allows for certain functions to propagate type information back to their arguments.
    pub(crate) fn feedback(&self, args: &[NodeIx], res: NodeIx, ctx: &mut types::TypeContext) {
        use types::{BaseTy, Constraint, TVar::*};
        if args.len() < self.arity().unwrap_or(0) {
            return;
        }
        match self {
            Function::Split => {
                let arg1 = ctx.constant(
                    Map {
                        key: BaseTy::Int,
                        val: BaseTy::Str,
                    }
                        .abs(),
                );
                ctx.nw.add_dep(arg1, args[1], Constraint::Flows(()));
            }
            Function::SplitSeps | Function::PatSplit => {
                for arg in [args[1], args[3]] {
                    let map = ctx.constant(
                        Map {
                            key: BaseTy::Int,
                            val: BaseTy::Str,
                        }
                            .abs(),
                    );
                    ctx.nw.add_dep(map, arg, Constraint::Flows(()));
                }
            }
            // headers of http_get/http_post and the payload of jwt are string maps
            Function::HttpGet | Function::HttpPost | Function::Jwt => {
                let map = ctx.constant(
                    Map {
                        key: BaseTy::Str,
                        val: BaseTy::Str,
                    }
                        .abs(),
                );
                ctx.nw.add_dep(map, args[args.len() - 1], Constraint::Flows(()));
            }
            // the positional parameters of the database functions
            Function::SqliteQuery | Function::SqliteExecute
            | Function::MysqlQuery | Function::MysqlExecute | Function::PgQuery | Function::PgExecute => {
                let map = ctx.constant(
                    Map {
                        key: BaseTy::Int,
                        val: BaseTy::Str,
                    }
                        .abs(),
                );
                ctx.nw.add_dep(map, args[2], Constraint::Flows(()));
            }
            // `arr[n, "start"]` and `arr[n, "length"]` are string keys.
            Function::MatchArr => {
                let arg2 = ctx.constant(
                    Map {
                        key: BaseTy::Str,
                        val: BaseTy::Str,
                    }
                        .abs(),
                );
                ctx.nw.add_dep(arg2, args[2], Constraint::Flows(()));
            }
            // asort(src, dst, how): both are maps, the values of `src` flow into `dst` so they share a
            // value type, while their key types are independent.
            Function::Asort => {
                let (src, dst) = (args[0], args[1]);
                ctx.constrain_as_map(src);
                ctx.constrain_as_map(dst);
                let val = ctx.fresh_var();
                for map in [src, dst] {
                    ctx.nw.add_dep(map, val, Constraint::Val(()));
                    ctx.nw.add_dep(val, map, Constraint::ValIn(()));
                }
            }
            // asorti(src, dst, how): the indices of `src` become the (string) values of `dst`.
            Function::Asorti => {
                ctx.constrain_as_map(args[0]);
                let dst = ctx.constant(Some(Map {
                    key: None,
                    val: Some(BaseTy::Str),
                }));
                ctx.nw.add_dep(dst, args[1], Constraint::Flows(()));
            }
            Function::ArrayStat
            | Function::ArrayCountDistinct
            | Function::ArrayTop
            | Function::ArrayMax
            | Function::ArrayMin
            | Function::ArraySum
            | Function::ArrayMean => {
                ctx.constrain_as_map(args[0]);
            }
            Function::Clear => {
                let is_map = ctx.constant(Some(Map {
                    key: None,
                    val: None,
                }));
                ctx.nw.add_dep(is_map, args[0], Constraint::Flows(()));
            }
            // An untyped argument of `_join` or `to_csv` becomes an (empty) array with integer keys.
            Function::IntMapJoin | Function::ToCsv => {
                let is_map = ctx.constant(Some(Map {
                    key: Some(BaseTy::Int),
                    val: None,
                }));
                ctx.nw.add_dep(is_map, args[0], Constraint::Flows(()));
            }
            Function::Contains => {
                let arr = args[0];
                let query = args[1];
                ctx.nw.add_dep(query, arr, Constraint::KeyIn(()));
            }
            Function::Delete => {
                let arr = args[0];
                let query = args[1];
                ctx.nw.add_dep(query, arr, Constraint::KeyIn(()));
            }
            Function::IncMap => {
                let arr = args[0];
                let k = args[1];
                let v = res;
                ctx.nw.add_dep(k, arr, Constraint::KeyIn(()));
                ctx.nw.add_dep(v, arr, Constraint::ValIn(()));
                ctx.nw.add_dep(arr, v, Constraint::Val(()));
            }
            // TODO: GenSub?
            Function::Sub | Function::GSub => {
                let out_str = args[2];
                let str_const = ctx.constant(Scalar(BaseTy::Str).abs());
                ctx.nw.add_dep(str_const, out_str, Constraint::Flows(()));
            }
            _ => {}
        };
    }
    pub(crate) fn type_sig(
        &self,
        incoming: &[compile::Ty],
        // TODO make the return type optional?
    ) -> Result<(SmallVec<compile::Ty>, compile::Ty)> {
        use {
            ast::{Binop::*, Unop::*},
            compile::Ty::*,
            Function::*,
        };
        if let Some(a) = self.arity() {
            if incoming.len() != a {
                let name = self.awk_name().map_or_else(|| self.to_string(), String::from);
                return Err(arity_error(&name, incoming.len(), a));
            }
        }
        fn arith_sig(x: compile::Ty, y: compile::Ty) -> (SmallVec<compile::Ty>, compile::Ty) {
            use compile::Ty::*;
            match (x, y) {
                (Str, _) | (_, Str) | (Float, _) | (_, Float) => (smallvec![Float; 2], Float),
                (_, _) => (smallvec![Int; 2], Int),
            }
        }
        Ok(match self {
            FloatFunc(ff) => ff.sig(),
            IntFunc(bw) => bw.sig(),
            Unop(Neg) | Unop(Pos) => match &incoming[0] {
                Str | Float => (smallvec![Float], Float),
                _ => (smallvec![Int], Int),
            },
            Unop(Column) => (smallvec![Int], Str),
            // A chain of concatenations is one call (see `View::convert_concat`).
            Binop(Concat) => (smallvec![Str; incoming.len()], Str),
            SubstrIndex | SubstrLastIndex | Binop(IsMatch) => (smallvec![Str; 2], Int),
            // Not doesn't convert its operand to an integer before negating it, as in awk: a
            // number is true when it is nonzero, and a string when it is nonempty (or, for a
            // strnum, nonzero):
            //    !5 == !1 == 0
            //    !0 == 1
            //    !"hi" == 0
            //    !(0.25) == 0
            // An unassigned variable is 0 (and ""), so `!x` is 1.
            Unop(Not) => match &incoming[0] {
                Int | Null => (smallvec![Int], Int),
                Float => (smallvec![Float], Int),
                Str => (smallvec![Str], Int),
                _ => return err!("unexpected input to Not: {:?}", incoming),
            },
            // String operands stay strings: whether they compare as strings or numbers depends
            // on their value and origin (awk's "strnum" rules, see runtime::compare).
            Binop(LT) | Binop(GT) | Binop(LTE) | Binop(GTE) | Binop(EQ) => (
                match (incoming[0], incoming[1]) {
                    (Str, Str) | (Str, Null) | (Null, Str) => smallvec![Str; 2],
                    (Int, Int) | (Null, Int) | (Int, Null) | (Null, Null) => smallvec![Int; 2],
                    (Str, Int | Float) => smallvec![Str, Float],
                    (Int | Float, Str) => smallvec![Float, Str],
                    (Float, _) | (_, Float) => smallvec![Float; 2],
                    _ => return err!("invalid input spec for comparison op: {:?}", incoming),
                },
                Int,
            ),
            LastPart => (smallvec![Str, Str], Str),
            Min | Max => (smallvec![Str,Str,Str], Str),
            StrCmp => (smallvec![Str,Str], Int),
            Similar => (smallvec![Str, Str], Float),
            GetEnv => (smallvec![Str,Str], Str),
            DefaultIfEmpty => (smallvec![Str,Str], Str),
            AppendIfMissing | PrependIfMissing | RemoveIfEnd | RemoveIfBegin => (smallvec![Str,Str], Str),
            Quote | DoubleQuote => (smallvec![Str], Str),
            Seq => (smallvec![Float,Float,Float], MapIntFloat),
            Uniq => (smallvec![MapIntStr, Str], MapIntStr),
            // Products of integers easily exceed the 64-bit range (e.g. factorials), and awk
            // numbers are doubles: multiply in floating point.
            Binop(Mult) => (smallvec![Float; 2], Float),
            Binop(Plus) | Binop(Minus) | Binop(Mod) => arith_sig(incoming[0], incoming[1]),
            Binop(Pow) | Binop(Div) => (smallvec![Float;2], Float),
            // A scalar where an array is expected is reported by type inference, which knows
            // where it comes from.
            Contains | Delete => match incoming[0] {
                MapIntInt | MapIntStr | MapIntFloat => (smallvec![incoming[0], Int], Int),
                _ => (smallvec![if incoming[0].is_array() { incoming[0] } else { MapStrStr }, Str], Int),
            },
            IncMap => {
                let map = incoming[0];
                if !map.is_array() {
                    return err!(
                        "first argument to inc_map must be an array type, got: {:?}",
                        map
                    );
                }
                let val = map.val().unwrap();
                let (args, res) = arith_sig(incoming[2], val);
                (
                    smallvec![incoming[0], incoming[0].key().unwrap(), args[0]],
                    res,
                )
            }
            Clear => (smallvec![if incoming[0].is_array() { incoming[0] } else { MapStrStr }], Int),
            Srand => (smallvec![Int], Int),
            System | HexToInt => (smallvec![Str], Int),
            System2 => (smallvec![Str], MapStrStr),
            ReseedRng => (smallvec![], Int),
            Rand => (smallvec![], Float),
            ToInt => {
                let inc = incoming[0];
                // `int(x)` truncates toward zero. A float or a string (converted with the usual
                // numeric rules) stays a float, so values outside the i64 range keep their value.
                match inc {
                    Null | Int => (smallvec![inc], Int),
                    Float | Str => (smallvec![inc], Float),
                    _ => {
                        return err!(
                            "can only convert scalar values to integers, got input with type: {:?}",
                            inc
                        );
                    }
                }
            }
            NextlineCmd | Nextline => (smallvec![Str], Str),
            ReadErrCmd | ReadErr => (smallvec![Str], Int),
            UpdateUsedFields | NextFile | ReadLineStdinFused => (smallvec![], Int),
            NextlineStdin => (smallvec![], Str),
            ReadErrStdin => (smallvec![], Int),
            // irrelevant return type
            Setcol => (smallvec![Int, Str], Int),
            Strlen => (smallvec![Str], Int),
            Length => match incoming[0] {
                // scalars are converted to strings first, as POSIX specifies
                Int | Float | Null => (smallvec![Str], Int),
                other => (smallvec![other], Int),
            },
            Uuid => (smallvec![Str], Str),
            SnowFlake => (smallvec![Int], Int),
            Ulid | Tsid => (smallvec![], Str),
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome => (smallvec![], Str),
            LocalIp => (smallvec![], Str),
            Systime => (smallvec![], Int),
            Strftime => (smallvec![Str, Int, Int], Str),
            Mktime => (smallvec![Str, Int, Int], Int),
            Duration => (smallvec![Str], Int),
            MkBool => (smallvec![Str], Int),
            MkPassword => (smallvec![Int], Str),
            Fend => (smallvec![Str], Str),
            Eval => (smallvec![Str, incoming[1]], Float),
            Url | Path | SemVer => (smallvec![Str], MapStrStr),
            Pairs => (smallvec![Str,Str,Str], MapStrStr),
            Parse => (smallvec![Str, Str], MapStrStr),
            RegexParse => (smallvec![Str, Str], MapIntStr),
            Record => (smallvec![Str], MapStrStr),
            Message => (smallvec![Str], MapStrStr),
            DataUrl => (smallvec![Str], MapStrStr),
            DateTime => (smallvec![Str], MapStrInt),
            Shlex => (smallvec![Str], MapIntStr),
            Tuple => (smallvec![Str], MapIntStr),
            Flags => (smallvec![Str], MapStrInt),
            ParseArray => (smallvec![Str], MapIntStr),
            Hex2Rgb => (smallvec![Str], MapIntInt),
            Rgb2Hex => (smallvec![Int, Int, Int], Str),
            Variant => (smallvec![Str], MapStrStr),
            Func => (smallvec![Str], MapIntStr),
            HttpGet => (smallvec![Str, MapStrStr], MapStrStr),
            HttpPost => (smallvec![Str, Str, MapStrStr], MapStrStr),
            SendMail => (smallvec![Str, Str, Str, Str, Str], Null),
            SmtpSend => (smallvec![Str, Str, Str, Str, Str], Null),
            S3Get => (smallvec![Str, Str], Str),
            S3Put => (smallvec![Str, Str, Str], Str),
            KvGet => (smallvec![Str, Str ], Str),
            KvPut => (smallvec![Str, Str,Str], Null),
            KvDelete => (smallvec![Str, Str], Null),
            KvClear => (smallvec![Str], Null),
            LogDebug | LogInfo | LogWarn | LogError => (smallvec![Str], Null),
            SqliteQuery | MysqlQuery | PgQuery => (smallvec![Str, Str, MapIntStr], MapIntStr),
            SqliteExecute | MysqlExecute | PgExecute => (smallvec![Str, Str, MapIntStr], Int),
            Publish => (smallvec![Str, Str], Null),
            FromJson => (smallvec![Str], MapStrStr),
            ToJson => (smallvec![incoming[0]], Str),
            JsonValue => (smallvec![Str,Str], Str),
            JsonQuery => (smallvec![Str,Str], MapIntStr),
            JsonHas => (smallvec![Str,Str], Int),
            JsonGet | JsonDel => (smallvec![Str,Str], Str),
            JsonSet => (smallvec![Str,Str,Str], Str),
            HtmlValue => (smallvec![Str,Str], Str),
            HtmlQuery => (smallvec![Str,Str], MapIntStr),
            XmlValue => (smallvec![Str,Str], Str),
            XmlQuery => (smallvec![Str,Str], MapIntStr),
            VarDump => (smallvec![incoming[0]], Null),
            ReadAll => (smallvec![Str], Str),
            WriteAll => (smallvec![Str, Str], Null),
            ReadConfig => (smallvec![Str], MapStrStr),
            FromCsv => (smallvec![Str], MapIntStr),
            // Only arrays with integer keys can be converted to CSV.
            ToCsv => match incoming[0] {
                MapIntInt | MapIntStr | MapIntFloat => (smallvec![incoming[0]], Str),
                _ => (smallvec![MapIntStr], Str),
            },
            Trim => (smallvec![Str, Str], Str),
            Truncate => (smallvec![Str, Int, Str], Str),
            Strtonum | AwkStrtonum => (smallvec![Str], Float),
            FormatBytes => (smallvec![Int], Str),
            ToBytes => (smallvec![Str], Int),
            StartsWith => (smallvec![Str, Str], Int),
            EndsWith => (smallvec![Str, Str], Int),
            TextContains => (smallvec![Str, Str], Int),
            Capitalize | UnCapitalize | CamelCase | KebabCase | SnakeCase | TitleCase | PascalCase | Figlet => (smallvec![Str], Str),
            PadLeft | PadRight | PadBoth => (smallvec![Str, Int, Str], Str),
            Mask => (smallvec![Str], Str),
            Repeat => (smallvec![Str, Int], Str),
            Words => (smallvec![Str], MapIntStr),
            Lines => (smallvec![Str], MapIntStr),
            Escape => (smallvec![Str, Str], Str),
            Encode => (smallvec![Str, Str], Str),
            Decode => (smallvec![Str, Str], Str),
            Digest => (smallvec![Str, Str], Str),
            Hmac => (smallvec![Str, Str, Str], Str),
            Jwt => (smallvec![Str, Str, MapStrStr], Str),
            Dejwt => (smallvec![Str, Str], MapStrStr),
            Encrypt => (smallvec![Str, Str, Str], Str),
            Decrypt => (smallvec![Str, Str, Str], Str),
            Asort => (smallvec![incoming[0], incoming[1], Str], Int),
            ArrayStat | ArrayCountDistinct | ArrayTop if !incoming[0].is_array() => {
                return err!("{} expects an array, got {:?}", self, incoming[0]);
            }
            ArrayMedian | ArrayQuantile | ArrayVar | ArrayStddev | ArrayMode => {
                return err!("{} should have been lowered to ArrayStat", self);
            }
            ArrayStat => (smallvec![incoming[0], Int, Float], Float),
            ArrayCountDistinct => (smallvec![incoming[0]], Int),
            ArrayTop => (smallvec![incoming[0], Int], MapIntStr),
            Asorti => {
                if incoming[0].is_array() && matches!(incoming[1], MapIntStr | MapStrStr) {
                    (smallvec![incoming[0], incoming[1], Str], Int)
                } else {
                    return err!("invalid input spec for asorti: {:?}", incoming);
                }
            }
            BloomFilterInsert => (smallvec![Str, Str], Null),
            BloomFilterContains | BloomFilterContainsWithInsert => (smallvec![Str, Str], Int),
            Fake => (smallvec![Str, Str], Str),
            Jev => (smallvec![Str, Str, Float], Int),
            JevProb => (smallvec![Str, Str], Float),
            JevChoice => (smallvec![Str, Str, Str], Str),
            JevScore => (smallvec![Str, Str, Str], Float),
            TypeOfVariable | TypeOfUntyped => (smallvec![incoming[0]], Str),
            IsArray => (smallvec![incoming[0]], Int),
            IsInt => (smallvec![incoming[0]], Int),
            IsNum => (smallvec![incoming[0]], Int),
            IsFormat => (smallvec![Str, Str], Int),
            // Only arrays with integer keys can be joined.
            IntMapJoin => match incoming[0] {
                MapIntInt | MapIntStr | MapIntFloat => (smallvec![incoming[0], Str], Str),
                _ => (smallvec![MapIntStr, Str], Str),
            },
            // map[int]int and map[int]float have fast paths; any other array is computed by
            // runtime::array_stats (values converted to numbers), as is the mean of map[int]int.
            ArrayMax | ArrayMin | ArraySum | ArrayMean => match incoming[0] {
                MapIntInt if *self != ArrayMean => (smallvec![incoming[0]], Int),
                ty if ty.is_array() => (smallvec![incoming[0]], Float),
                ty => return err!("{} expects an array of numbers, got {:?}", self, ty),
            },
            Close => (smallvec![Str], Int),
            Fflush => (smallvec![Str], Int),
            Strnum => (smallvec![Str], Str),
            ToFloat => (smallvec![Float], Float),
            OfmtStr => (
                smallvec![if matches!(incoming[0], Int | Float) { incoming[0] } else { Str }],
                Str,
            ),
            Sub | GSub => (smallvec![Str, Str, Str], Int),
            GenSub => (smallvec![Str, Str, Str, Str], Str),
            ToUpper | ToLower | EscapeCSV | EscapeTSV => (smallvec![Str], Str),
            Substr => (smallvec![Str, Int, Int], Str),
            CharAt => (smallvec![Str, Int], Str),
            Chars => (smallvec![Str], MapIntStr),
            Match => (smallvec![Str, Str], Int),
            // match(s, re, arr, SUBSEP)
            MatchArr => {
                if let MapStrStr = incoming[2] {
                    (smallvec![Str, Str, MapStrStr, Str], Int)
                } else {
                    return err!("invalid input spec for match: {:?}", incoming);
                }
            }
            Exit => (smallvec![Int], Null),
            SetExit => (smallvec![Int, Int], Int),
            Exiting => (smallvec![], Int),
            // Split's second input can be a map of either type
            Split => {
                if let MapIntStr | MapStrStr = incoming[1] {
                    (smallvec![Str, incoming[1], Str], Int)
                } else {
                    return err!("invalid input spec for split: {:?}", incoming);
                }
            }
            SplitSeps | PatSplit => {
                if let (MapIntStr | MapStrStr, MapIntStr | MapStrStr) = (incoming[1], incoming[3]) {
                    (smallvec![Str, incoming[1], Str, incoming[3]], Int)
                } else {
                    return err!("invalid input spec for split: {:?}", incoming);
                }
            }
            JoinCols => (smallvec![Int, Int, Str], Str),
            JoinCSV | JoinTSV => (smallvec![Int, Int], Str),
            SetFI => (smallvec![Int, Int], Int),
        })
    }

    pub(crate) fn arity(&self) -> Option<usize> {
        use Function::*;
        if let Binop(ast::Binop::Concat) = self {
            // Two operands or more: a chain of concatenations is one call.
            return None;
        }
        Some(match self {
            FloatFunc(ff) => ff.arity(),
            IntFunc(bw) => bw.arity(),
            UpdateUsedFields | Rand | Ulid | Tsid | LocalIp | Systime | ReseedRng | ReadErrStdin | NextlineStdin | NextFile
            | ReadLineStdinFused | Exiting => 0,
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome => 0,
            Exit | ToUpper | ToLower | Clear | Srand | System | System2 | HexToInt | ToInt | EscapeCSV
            | EscapeTSV | Close | Fflush | Length | Strlen | ReadErr | ReadErrCmd | Nextline | NextlineCmd
            | Uuid | SnowFlake | Fend | Url | SemVer | Path | DataUrl | DateTime | Shlex | Tuple | Variant | Flags | ParseArray | Func | ToJson | FromJson | ToCsv | FromCsv | TypeOfVariable | TypeOfUntyped | IsArray | Unop(_) => 1,
            SetFI | SetExit | SubstrIndex | SubstrLastIndex | Match | Setcol | Binop(_) => 2,
            MatchArr => 4,
            JoinCSV | JoinTSV | Delete | Contains => 2,
            Eval => 2,
            DefaultIfEmpty => 2,
            JsonValue | JsonQuery | HtmlValue | HtmlQuery | XmlValue | XmlQuery => 2,
            JsonHas | JsonGet | JsonDel => 2,
            JsonSet => 3,
            AppendIfMissing | PrependIfMissing | RemoveIfEnd | RemoveIfBegin => 2,
            Pairs => 3,
            LastPart => 2,
            Hex2Rgb => 1,
            Rgb2Hex => 3,
            GetEnv => 2,
            Parse | RegexParse => 2,
            Record | Message => 1,
            Quote | DoubleQuote => 1,
            VarDump => 1,
            FormatBytes | ToBytes => 1,
            StartsWith | EndsWith | TextContains => 2,
            ReadAll | ReadConfig => 1,
            WriteAll => 2,
            Dejwt => 2,
            BloomFilterInsert | BloomFilterContains | BloomFilterContainsWithInsert => 2,
            Fake => 2,
            JevProb => 2,
            Jev | JevChoice | JevScore => 3,
            Encrypt | Decrypt => 3,
            Strftime => 3,
            Mktime => 3,
            Duration => 1,
            StrCmp => 2,
            Similar => 2,
            CharAt => 2,
            Chars => 1,
            MkBool => 1,
            MkPassword => 1,
            Trim => 2,
            Capitalize | UnCapitalize | Mask | Strtonum | AwkStrtonum | CamelCase | KebabCase | SnakeCase | TitleCase | PascalCase | Words | Lines => 1,
            Figlet => 1,
            Repeat => 2,
            Min | Max => 3,
            Seq => 3,
            Uniq => 2,
            Strnum | ToFloat | OfmtStr => 1,
            Asort => 3,
            Asorti => 3,
            ArrayMedian | ArrayVar | ArrayStddev | ArrayMode | ArrayCountDistinct => 1,
            ArrayQuantile | ArrayTop => 2,
            ArrayStat => 3,
            SplitSeps | PatSplit => 4,
            HttpGet => 2,
            HttpPost => 3,
            SendMail | SmtpSend => 5,
            S3Get => 2,
            S3Put => 3,
            KvGet | KvDelete => 2,
            KvPut => 3,
            KvClear => 1,
            SqliteQuery | SqliteExecute | MysqlQuery | MysqlExecute | PgQuery | PgExecute => 3,
            PadLeft | PadRight | PadBoth => 3,
            Publish => 2,
            IsInt | IsNum => 1,
            IsFormat => 2,
            Encode | Decode | Digest | Escape => 2,
            Hmac | Jwt => 3,
            LogDebug | LogInfo | LogWarn | LogError => 1,
            ArrayMax | ArrayMin | ArraySum | ArrayMean => 1,
            IntMapJoin => 2,
            IncMap | JoinCols | Substr | Sub | GSub | Split | Truncate => 3,
            GenSub => 4,
        })
    }

    pub(crate) fn step(&self, args: &[types::State]) -> Result<types::State> {
        use {
            ast::{Binop::*, Unop::*},
            types::{BaseTy, TVar::*},
            Function::*,
        };
        fn step_arith(x: &types::State, y: &types::State) -> types::State {
            use BaseTy::*;
            match (x, y) {
                (Some(Scalar(Some(Str | Float))), _) | (_, Some(Scalar(Some(Str | Float)))) => {
                    Scalar(Float).abs()
                }
                (_, _) => Scalar(Int).abs(),
            }
        }
        match self {
            IntFunc(bw) => Ok(bw.ret_state()),
            FloatFunc(ff) => Ok(ff.ret_state()),
            ToInt => match &args[0] {
                Some(Scalar(Some(BaseTy::Str))) | Some(Scalar(Some(BaseTy::Float))) => {
                    Ok(Scalar(BaseTy::Float).abs())
                }
                _ => Ok(Scalar(BaseTy::Int).abs()),
            },
            // The result is a number, also for an unassigned variable: `-x` is 0, not "".
            Unop(Neg) | Unop(Pos) => match &args[0] {
                Some(Scalar(Some(BaseTy::Str))) | Some(Scalar(Some(BaseTy::Float))) => {
                    Ok(Scalar(BaseTy::Float).abs())
                }
                _ => Ok(Scalar(BaseTy::Int).abs()),
            },
            Binop(Plus) | Binop(Minus) | Binop(Mod) => Ok(step_arith(&args[0], &args[1])),
            Min | Max => Ok(Scalar(BaseTy::Str).abs()),
            Rand | Binop(Div) | Binop(Pow) | Binop(Mult) => Ok(Scalar(BaseTy::Float).abs()),
            Setcol => Ok(Scalar(BaseTy::Null).abs()),
            Clear | SubstrIndex | SubstrLastIndex | Srand | ReseedRng | Unop(Not) | Binop(IsMatch) | Binop(LT)
            | Binop(GT) | Binop(LTE) | Binop(GTE) | Binop(EQ) | Length | Strlen | Split | SplitSeps | PatSplit | ReadErr
            | ReadErrCmd | ReadErrStdin | Contains | Delete | Match | MatchArr | Sub | GSub | Systime | Mktime | Duration
            | System | HexToInt | Asort | Asorti | MkBool | SnowFlake | ArrayCountDistinct | Exiting => Ok(Scalar(BaseTy::Int).abs()),
            ArrayMedian | ArrayQuantile | ArrayVar | ArrayStddev | ArrayMode | ArrayStat => Ok(Scalar(BaseTy::Float).abs()),
            ArrayTop => Ok(Map {
                key: BaseTy::Int,
                val: BaseTy::Str,
            }.abs()),
            System2 => Ok(Map {
                key: BaseTy::Str,
                val: BaseTy::Str,
            }.abs()),
            ToUpper | ToLower | JoinCSV | JoinTSV | Uuid | Ulid | Tsid | LocalIp | Strftime | Fend | Trim | Truncate | JoinCols
            | EscapeCSV | EscapeTSV | Escape
            | Unop(Column) | Binop(Concat) | Nextline | NextlineCmd | NextlineStdin | GenSub | Substr | CharAt
            | Encode | Decode | Digest | Hmac | Jwt | ToJson | JsonValue | JsonGet | JsonSet | JsonDel | HtmlValue | XmlValue | ToCsv | TypeOfVariable | TypeOfUntyped | IntMapJoin => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            Chars => Ok(Map {
                key: BaseTy::Int,
                val: BaseTy::Str,
            }.abs()),
            Eval => Ok(Scalar(BaseTy::Float).abs()),
            JsonQuery | HtmlQuery | XmlQuery => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            MkPassword => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            Encrypt | Decrypt => Ok(Scalar(BaseTy::Str).abs()),
            Fake => Ok(Scalar(BaseTy::Str).abs()),
            Jev => Ok(Scalar(BaseTy::Int).abs()),
            JevProb | JevScore => Ok(Scalar(BaseTy::Float).abs()),
            JevChoice => Ok(Scalar(BaseTy::Str).abs()),
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome | GetEnv => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            LastPart => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            FormatBytes => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            ToBytes => {
                Ok(Scalar(BaseTy::Int).abs())
            }
            StartsWith | EndsWith | TextContains | JsonHas => {
                Ok(Scalar(BaseTy::Int).abs())
            }
            BloomFilterInsert => Ok(None),
            BloomFilterContains | BloomFilterContainsWithInsert => {
                Ok(Scalar(BaseTy::Int).abs())
            }
            Strtonum | AwkStrtonum => Ok(Scalar(BaseTy::Float).abs()),
            Capitalize | UnCapitalize | Mask | CamelCase | KebabCase | SnakeCase | TitleCase | PascalCase | Figlet | Repeat => Ok(Scalar(BaseTy::Str).abs()),
            DefaultIfEmpty => Ok(Scalar(BaseTy::Str).abs()),
            AppendIfMissing | PrependIfMissing | RemoveIfEnd | RemoveIfBegin => Ok(Scalar(BaseTy::Str).abs()),
            Quote | DoubleQuote => Ok(Scalar(BaseTy::Str).abs()),
            IsArray | IsNum | IsInt | IsFormat => Ok(Scalar(BaseTy::Int).abs()),
            Url | SemVer | Path | DataUrl | Dejwt | Pairs | Record | Message => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Str,
                }.abs())
            }
            Words | Lines => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            DateTime => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Int,
                }.abs())
            }
            RegexParse => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            Parse => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Str,
                }.abs())
            }
            Shlex | Func | Tuple | ParseArray => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            Hex2Rgb => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Int,
                }.abs())
            }
            Rgb2Hex => {
                Ok(Scalar(BaseTy::Str).abs())
            }
            Flags => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Int,
                }.abs())
            }
            Variant => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Str,
                }.abs())
            }
            SqliteQuery | MysqlQuery | PgQuery  => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            SqliteExecute | MysqlExecute | PgExecute => Ok(Scalar(BaseTy::Int).abs()),
            Strnum => Ok(Scalar(BaseTy::Str).abs()),
            ToFloat => Ok(Scalar(BaseTy::Float).abs()),
            OfmtStr => Ok(Scalar(BaseTy::Str).abs()),
            Uniq => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            HttpGet | HttpPost => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Str,
                }.abs())
            }
            SmtpSend | SendMail => Ok(None),
            S3Get | S3Put => Ok(Scalar(BaseTy::Str).abs()),
            FromJson => {
                Ok(Map {
                    key: BaseTy::Str,
                    val: BaseTy::Str,
                }.abs())
            }
            FromCsv => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            Seq => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Float,
                }.abs())
            }
            PadLeft | PadRight | PadBoth => Ok(Scalar(BaseTy::Str).abs()),
            ArrayMax | ArrayMin | ArraySum | ArrayMean => match &args[0] {
                Some(Map {
                         key: Some(BaseTy::Int),
                         val: Some(BaseTy::Int)
                     }) if *self != ArrayMean => {
                    Ok(Scalar(BaseTy::Int).abs())
                }
                _ => { Ok(Scalar(BaseTy::Float).abs()) }
            },
            StrCmp => Ok(Scalar(BaseTy::Int).abs()),
            Similar => Ok(Scalar(BaseTy::Float).abs()),
            IncMap => Ok(step_arith(&types::val_of(&args[0])?, &args[2])),
            Exit | SetExit | SetFI | UpdateUsedFields | NextFile | ReadLineStdinFused => Ok(None),
            Close | Fflush => Ok(Scalar(BaseTy::Int).abs()),
            KvGet => Ok(Scalar(BaseTy::Str).abs()),
            ReadAll => Ok(Scalar(BaseTy::Str).abs()),
            WriteAll => Ok(None),
            ReadConfig => Ok(Map {
                key: BaseTy::Str,
                val: BaseTy::Str,
            }.abs()),
            KvPut | KvDelete | KvClear => Ok(None),
            VarDump => Ok(None),
            LogDebug | LogInfo | LogWarn | LogError => Ok(None),
            Publish => Ok(None),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
// We may relax this in the future, but these names are all-caps here to match
// their names in Awk.
#[allow(clippy::upper_case_acronyms, non_camel_case_types)]
pub(crate) enum Variable {
    ARGC = 0,
    ARGV = 1,
    OFS = 2,
    FS = 3,
    RS = 4,
    NF = 5,
    NR = 6,
    FILENAME = 7,
    RSTART = 8,
    RLENGTH = 9,
    ORS = 10,
    FNR = 11,
    PID = 12,
    FI = 13,
    ENVIRON = 14,
    PROCINFO = 15,
    CONVFMT = 16,
    OFMT = 17,
    IGNORECASE = 18,
    ERRNO = 19,
    // The subject, headers and payload of the NATS message being read (`nats://` input).
    MSG_SUBJECT = 20,
    MSG_HEADERS = 21,
    MSG_BODY = 22,
}

impl From<Variable> for compile::Ty {
    fn from(v: Variable) -> compile::Ty {
        use Variable::*;
        match v {
            FS | OFS | ORS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_BODY => compile::Ty::Str,
            PID | ARGC | NF | NR | FNR | RSTART | RLENGTH | IGNORECASE => compile::Ty::Int,
            ARGV => compile::Ty::MapIntStr,
            FI => compile::Ty::MapStrInt,
            ENVIRON | PROCINFO | MSG_HEADERS => compile::Ty::MapStrStr,
        }
    }
}

pub(crate) struct Variables<'a> {
    pub argc: Int,
    pub argv: IntMap<Str<'a>>,
    pub fs: Str<'a>,
    // The pattern used to split records into fields; see `update_split_fs`.
    pub split_fs: Str<'a>,
    pub ofs: Str<'a>,
    pub ors: Str<'a>,
    pub rs: Str<'a>,
    pub nf: Int,
    pub nr: Int,
    pub fnr: Int,
    pub filename: Str<'a>,
    pub rstart: Int,
    pub rlength: Int,
    pub pid: Int,
    pub fi: StrMap<'a, Int>,
    // The column names of the header being read into FI, see `set_fi`.
    pub fi_header: hashbrown::HashSet<Vec<u8>>,
    pub environ: StrMap<'a, Str<'a>>,
    pub procinfo: StrMap<'a, Str<'a>>,
    // Whether the program references PROCINFO (set by the compiler). When it does not,
    // `PROCINFO["sorted_in"]` is never set, so `for (k in a)` does not look it up.
    pub uses_procinfo: bool,
    pub convfmt: Str<'a>,
    pub ofmt: Str<'a>,
    // gawk's IGNORECASE: regex matching ignores case when it is nonzero.
    pub ignorecase: Int,
    // The NATS message being read, see `splitter::nats`.
    pub msg_subject: Str<'a>,
    pub msg_headers: StrMap<'a, Str<'a>>,
    pub msg_body: Str<'a>,
}

impl<'a> Default for Variables<'a> {
    fn default() -> Variables<'a> {
        Variables {
            argc: 0,
            argv: Default::default(),
            fs: " ".into(),
            split_fs: " ".into(),
            ofs: " ".into(),
            ors: "\n".into(),
            rs: "\n".into(),
            nr: 0,
            fnr: 0,
            nf: 0,
            filename: Default::default(),
            convfmt: "%.6g".into(),
            ofmt: "%.6g".into(),
            rstart: 0,
            pid: 0,
            rlength: 0,
            fi: Default::default(),
            fi_header: Default::default(),
            environ: load_env_variables(),
            procinfo: load_procinfo_variables(),
            uses_procinfo: true,
            ignorecase: 0,
            msg_subject: Default::default(),
            msg_headers: Default::default(),
            msg_body: Default::default(),
        }
    }
}

fn load_env_variables<'a>() -> StrMap<'a, Str<'a>> {
    let env = StrMap::default();
    for (k, v) in std::env::vars() {
        env.insert(k.into(), v.into());
    }
    crate::runtime::command::register_environ(&env);
    env
}

/// The contents of PROCINFO, as in gawk (without gawk's internal and array-valued entries).
fn load_procinfo_variables<'a>() -> StrMap<'a, Str<'a>> {
    let procinfo = StrMap::default();
    let set = |k: &'static str, v: String| procinfo.insert(k.into(), v.into());
    set("version", VERSION.into());
    // The default format of strftime().
    set("strftime", "%a %b %e %H:%M:%S %Z %Y".into());
    // How fields are split: zawk only supports FS (not FIELDWIDTHS or FPAT).
    set("FS", "FS".into());
    set("pid", std::process::id().to_string());
    #[cfg(target_family = "unix")]
    {
        set("platform", "posix".into());
        unsafe {
            set("uid", libc::getuid().to_string());
            set("gid", libc::getgid().to_string());
            set("euid", libc::geteuid().to_string());
            set("egid", libc::getegid().to_string());
            set("pgrpid", libc::getpgrp().to_string());
            set("ppid", libc::getppid().to_string());
            // The supplementary groups: group1, group2, ...
            let n = libc::getgroups(0, std::ptr::null_mut());
            if n > 0 {
                let mut groups = vec![0 as libc::gid_t; n as usize];
                let n = libc::getgroups(n, groups.as_mut_ptr());
                for (i, g) in groups.iter().take(n.max(0) as usize).enumerate() {
                    procinfo.insert(format!("group{}", i + 1).into(), g.to_string().into());
                }
            }
        }
    }
    #[cfg(target_family = "windows")]
    set("platform", "windows".into());
    procinfo
}

impl<'a> Variables<'a> {
    /// Sets `FI[name] = col` for column `col` of a header (`-H`). A name repeated in the header
    /// gets the first free `_1`, `_2`, ... suffix (`a,a,b` -> `a`, `a_1`, `b`), as for JSON and
    /// Parquet input (see `splitter::unique_names`), so every column can be reached by name.
    pub fn set_fi(&mut self, name: Str<'a>, col: Int) {
        if col == 1 {
            self.fi_header.clear();
        }
        let base = name.with_bytes(|bs| bs.to_vec());
        let mut unique = name.clone();
        let mut key = base.clone();
        let mut n = 0;
        while self.fi_header.contains(&key) {
            n += 1;
            let suffix = format!("_{}", n);
            key.truncate(base.len());
            key.extend_from_slice(suffix.as_bytes());
            unique = Str::concat(name.clone(), suffix.into());
        }
        self.fi_header.insert(key);
        self.fi.insert(unique, col);
    }

    /// Load the NATS message being read (if any) into MSG_SUBJECT, MSG_HEADERS and MSG_BODY. Each
    /// message is an input file of its own: this runs when the input file changes.
    pub(crate) fn load_message(&mut self) {
        let Some(msg) = crate::runtime::splitter::nats::current_message() else { return };
        self.msg_subject = msg.subject.clone().into();
        self.msg_body = msg.body.clone().into();
        let headers = StrMap::default();
        for (k, v) in msg.headers.iter() {
            headers.insert(k.clone().into(), v.clone().into());
        }
        self.msg_headers = headers;
    }

    /// The order of `for (k in a)`, from `PROCINFO["sorted_in"]`.
    pub(crate) fn sorted_in(&self) -> Result<crate::runtime::sorted_in::SortedIn> {
        if !self.uses_procinfo {
            return Ok(crate::runtime::sorted_in::SortedIn::UNSORTED);
        }
        crate::runtime::sorted_in::SortedIn::from_procinfo(&self.procinfo)
    }

    pub fn load_int(&self, var: Variable) -> Result<Int> {
        use Variable::*;
        Ok(match var {
            ARGC => self.argc,
            NF => self.nf,
            NR => self.nr,
            FNR => self.fnr,
            RSTART => self.rstart,
            RLENGTH => self.rlength,
            PID => self.pid,
            IGNORECASE => self.ignorecase,
            FI | ORS | OFS | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | ARGV | ENVIRON | PROCINFO => return err!("var {} not an int", var),
        })
    }

    pub fn store_int(&mut self, var: Variable, i: Int) -> Result<()> {
        use Variable::*;
        match var {
            ARGC => self.argc = i,
            NF => self.nf = i,
            NR => self.nr = i,
            FNR => self.fnr = i,
            RSTART => self.rstart = i,
            RLENGTH => self.rlength = i,
            PID => self.pid = i,
            IGNORECASE => {
                crate::runtime::awk_regex::set_ignore_case(i != 0);
                self.ignorecase = i;
            }
            FI | ORS | OFS | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | ARGV | ENVIRON | PROCINFO => return err!("var {} not an int", var),
        }
        Ok(())
    }

    pub fn load_str(&self, var: Variable) -> Result<Str<'a>> {
        use Variable::*;
        Ok(match var {
            FS => self.fs.clone(),
            OFS => self.ofs.clone(),
            ORS => self.ors.clone(),
            RS => self.rs.clone(),
            FILENAME => self.filename.clone(),
            CONVFMT => self.convfmt.clone(),
            OFMT => self.ofmt.clone(),
            ERRNO => crate::runtime::errno().into(),
            MSG_SUBJECT => self.msg_subject.clone(),
            MSG_BODY => self.msg_body.clone(),
            FI | PID | ARGC | ARGV | NF | NR | FNR | RSTART | RLENGTH | ENVIRON | PROCINFO | IGNORECASE | MSG_HEADERS => {
                return err!("var {} not a string", var);
            }
        })
    }

    /// Recompute the pattern used to split records into fields. It is FS, except in paragraph
    /// mode (RS = "") with a single-character FS, where a newline separates fields as well (as
    /// in gawk; this does not apply to regular expression FS values). The default FS (" ")
    /// already splits on newlines.
    pub fn update_split_fs(&mut self) {
        let fs = self.fs.as_str().to_string();
        self.split_fs = if self.rs.is_empty() && fs != " " && fs.chars().count() == 1 {
            // A single-character FS is a literal character.
            Str::from(format!("{}|\n", regex::escape(&fs)))
        } else {
            self.fs.clone()
        };
    }

    pub fn store_str(&mut self, var: Variable, s: Str<'a>) -> Result<()> {
        use Variable::*;
        match var {
            FS => {
                self.fs = s;
                self.update_split_fs();
            }
            OFS => self.ofs = s,
            ORS => self.ors = s,
            RS => {
                self.rs = s;
                self.update_split_fs();
            }
            FILENAME => self.filename = s,
            CONVFMT => {
                crate::runtime::numfmt::set_convfmt(&*s.as_str().as_bytes());
                self.convfmt = s;
            }
            OFMT => {
                crate::runtime::numfmt::set_ofmt(&*s.as_str().as_bytes());
                self.ofmt = s;
            }
            ERRNO => crate::runtime::set_errno(s.as_str().to_string()),
            MSG_SUBJECT => self.msg_subject = s,
            MSG_BODY => self.msg_body = s,
            FI | PID | ARGC | ARGV | NF | NR | FNR | RSTART | RLENGTH | ENVIRON | PROCINFO | IGNORECASE | MSG_HEADERS => {
                return err!("var {} not a string", var);
            }
        };
        Ok(())
    }

    pub fn load_intmap(&self, var: Variable) -> Result<IntMap<Str<'a>>> {
        use Variable::*;
        match var {
            ARGV => Ok(self.argv.clone()),
            FI | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | RSTART | RLENGTH | ENVIRON | PROCINFO | IGNORECASE => {
                err!("var {} is not an int-keyed map", var)
            }
        }
    }

    pub fn store_intmap(&mut self, var: Variable, m: IntMap<Str<'a>>) -> Result<()> {
        use Variable::*;
        match var {
            ARGV => {
                self.argv = m;
                Ok(())
            }
            FI | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | RSTART | RLENGTH | ENVIRON | PROCINFO | IGNORECASE => {
                err!("var {} is not an int-keyed map", var)
            }
        }
    }

    pub fn load_strmap(&self, var: Variable) -> Result<StrMap<'a, Int>> {
        use Variable::*;
        match var {
            FI => Ok(self.fi.clone()),
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | RSTART | ENVIRON | PROCINFO
            | RLENGTH | IGNORECASE => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }

    pub fn store_strmap(&mut self, var: Variable, m: StrMap<'a, Int>) -> Result<()> {
        use Variable::*;
        match var {
            FI => {
                self.fi = m;
                Ok(())
            }
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_HEADERS | MSG_BODY | RSTART | ENVIRON | PROCINFO
            | RLENGTH | IGNORECASE => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }

    pub fn load_strstrmap(&self, var: Variable) -> Result<StrMap<'a, Str<'a>>> {
        use Variable::*;
        match var {
            ENVIRON => Ok(self.environ.clone()),
            PROCINFO => Ok(self.procinfo.clone()),
            MSG_HEADERS => Ok(self.msg_headers.clone()),
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_BODY | RSTART | FI
            | RLENGTH | IGNORECASE => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }

    pub fn store_strstrmap(&mut self, var: Variable, m: StrMap<'a, Str<'a>>) -> Result<()> {
        use Variable::*;
        match var {
            ENVIRON => {
                crate::runtime::command::register_environ(&m);
                self.environ = m;
                Ok(())
            }
            PROCINFO => {
                self.procinfo = m;
                Ok(())
            }
            MSG_HEADERS => {
                self.msg_headers = m;
                Ok(())
            }
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_BODY | RSTART | FI
            | RLENGTH | IGNORECASE => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }
}

impl Variable {
    /// Whether the variable is an array, such as ARGV or ENVIRON.
    pub(crate) fn is_array(&self) -> bool {
        matches!(self.ty(), types::TVar::Map { .. })
    }

    pub(crate) fn ty(&self) -> types::TVar<types::BaseTy> {
        use Variable::*;
        match self {
            PID | ARGC | NF | FNR | NR | RSTART | RLENGTH | IGNORECASE => {
                types::TVar::Scalar(types::BaseTy::Int)
            }
            // NB: For full compliance, this may have to be Str -> Str
            //  If we had
            //  m["x"] = 1;
            //  if (true) {
            //      m = ARGV
            //  }
            //  I think we have SSA:
            //  L0:
            //    m0["x"] = 1;
            //    jmpif false L2
            //  L1:
            //    m1 = ARGV
            //  L2:
            //    m2 = phi [L0: m0, L1: m1]
            //
            //  And m0 and m1 have to be the same type, because we do not want to convert between map
            //  types.
            //  I think the solution here is just to have ARGV be a local variable. It doesn't
            //  actually have to be a builtin.
            //
            //  OTOH... maybe it's not so bad that we get type errors when putting strings as keys
            //  in ARGV.
            ARGV => types::TVar::Map {
                key: types::BaseTy::Int,
                val: types::BaseTy::Str,
            },
            FI => types::TVar::Map {
                key: types::BaseTy::Str,
                val: types::BaseTy::Int,
            },
            ENVIRON | PROCINFO | MSG_HEADERS => types::TVar::Map {
                key: types::BaseTy::Str,
                val: types::BaseTy::Str,
            },
            ORS | OFS | FS | RS | FILENAME | CONVFMT | OFMT | ERRNO | MSG_SUBJECT | MSG_BODY => types::TVar::Scalar(types::BaseTy::Str),
        }
    }
}

impl<'a> TryFrom<&'a str> for Variable {
    type Error = ();
    // error means not found
    fn try_from(value: &'a str) -> std::result::Result<Variable, ()> {
        match VARIABLES.get(value) {
            Some(v) => Ok(*v),
            None => Err(()),
        }
    }
}

impl TryFrom<usize> for Variable {
    type Error = ();
    // error means not found
    fn try_from(value: usize) -> std::result::Result<Variable, ()> {
        use Variable::*;
        match value {
            0 => Ok(ARGC),
            1 => Ok(ARGV),
            2 => Ok(OFS),
            3 => Ok(FS),
            4 => Ok(RS),
            5 => Ok(NF),
            6 => Ok(NR),
            7 => Ok(FILENAME),
            8 => Ok(RSTART),
            9 => Ok(RLENGTH),
            10 => Ok(ORS),
            11 => Ok(FNR),
            12 => Ok(PID),
            13 => Ok(FI),
            14 => Ok(ENVIRON),
            15 => Ok(PROCINFO),
            16 => Ok(CONVFMT),
            17 => Ok(OFMT),
            18 => Ok(IGNORECASE),
            19 => Ok(ERRNO),
            20 => Ok(MSG_SUBJECT),
            21 => Ok(MSG_HEADERS),
            22 => Ok(MSG_BODY),
            _ => Err(()),
        }
    }
}

static_map!(
    VARIABLES<&'static str, Variable>,
    ["ARGC", Variable::ARGC],
    ["ARGV", Variable::ARGV],
    ["OFS", Variable::OFS],
    ["ORS", Variable::ORS],
    ["FS", Variable::FS],
    ["RS", Variable::RS],
    ["NF", Variable::NF],
    ["NR", Variable::NR],
    ["FNR", Variable::FNR],
    ["FILENAME", Variable::FILENAME],
    ["RSTART", Variable::RSTART],
    ["RLENGTH", Variable::RLENGTH],
    ["PID", Variable::PID],
    ["FI", Variable::FI],
    ["ENVIRON", Variable::ENVIRON],
    ["PROCINFO", Variable::PROCINFO],
    ["CONVFMT", Variable::CONVFMT],
    ["OFMT", Variable::OFMT],
    ["IGNORECASE", Variable::IGNORECASE],
    ["ERRNO", Variable::ERRNO],
    ["MSG_SUBJECT", Variable::MSG_SUBJECT],
    ["MSG_HEADERS", Variable::MSG_HEADERS],
    ["MSG_BODY", Variable::MSG_BODY]
);
