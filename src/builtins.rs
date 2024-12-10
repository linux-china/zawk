//! This module contains definitions and metadata for builtin functions and builtin variables.
use crate::ast;
#[allow(unused_imports)]
use crate::common::Either;
use crate::common::{NodeIx, Result};
use crate::compile;
use crate::runtime::{Int, IntMap, Str, StrMap};
use crate::types::{self, SmallVec};
use smallvec::smallvec;

use std::convert::TryFrom;

pub const VERSION: &'static str = "0.5.22";

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Function {
    Unop(ast::Unop),
    Binop(ast::Binop),
    FloatFunc(FloatFunc),
    IntFunc(Bitwise),
    Close,
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
    Length,
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
    Strtonum,
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
    Figlet,
    PadLeft,
    PadRight,
    PadBoth,
    StrCmp,
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
    LibsqlQuery,
    LibsqlExecute,
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
    Asort,
    BloomFilterInsert,
    BloomFilterContains,
    BloomFilterContainsWithInsert,
    Fake,
    LocalIp,
    Contains,
    Delete,
    Clear,
    Match,
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
            LogicalRightShift => (lhs as usize).wrapping_shr(rhs as u32) as i64,
            ArithmeticRightShift => lhs.wrapping_shr(rhs as u32),
            LeftShift => lhs.wrapping_shl(rhs as u32),
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

// TODO: move the llvm-level code back into the LLVM module.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FloatFunc {
    Cos,
    Sin,
    Atan,
    Atan2,
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
            Atan2 => panic!("float: mismatched arity!"),
        }
    }
    pub fn eval2(&self, x: f64, y: f64) -> f64 {
        use FloatFunc::*;
        match self {
            Atan2 => x.atan2(y),
            Sqrt | Cos | Sin | Atan | Log | Log2 | Log10 | Exp | Abs | Ceil | Floor | Round => {
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
            Exp => "exp",
            Abs => "abs",
            Ceil => "ceil",
            Floor => "floor",
            Round => "round",
        }
    }

    pub fn arity(&self) -> usize {
        use FloatFunc::*;
        match self {
            Sqrt | Cos | Sin | Atan | Log | Log2 | Log10 | Exp | Abs | Ceil | Floor | Round => 1,
            Atan2 => 2,
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
    ["split", Function::Split],
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
    ["libsql_query", Function::LibsqlQuery],
    ["libsql_execute", Function::LibsqlExecute],
    ["mysql_query", Function::MysqlQuery],
    ["mysql_execute", Function::MysqlExecute],
    ["pg_query", Function::PgQuery],
    ["pg_execute", Function::PgExecute],
    ["publish", Function::Publish],
    ["from_json", Function::FromJson],
    ["to_json", Function::ToJson],
    ["json_value", Function::JsonValue],
    ["json_query", Function::JsonQuery],
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
    ["_join", Function::IntMapJoin],
    ["seq", Function::Seq],
    ["uniq", Function::Uniq],
    ["asort", Function::Asort],
    ["bf_insert", Function::BloomFilterInsert],
    ["bf_contains", Function::BloomFilterContains],
    ["bf_icontains", Function::BloomFilterContainsWithInsert],
    ["fake", Function::Fake],
    ["local_ip", Function::LocalIp],
    ["truncate", Function::Truncate],
    ["parse", Function::Parse],
    ["rparse", Function::RegexParse],
    ["strtonum", Function::Strtonum],
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
    ["pascal_case", Function::TitleCase],
    ["figlet", Function::Figlet],
    ["pad_end", Function::PadLeft],
    ["pad_start", Function::PadRight],
    ["pad", Function::PadBoth],
    ["strcmp", Function::StrCmp],
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

pub(crate) trait IsSprintf {
    fn is_sprintf(&self) -> bool;
}

impl<'a> IsSprintf for &'a str {
    fn is_sprintf(&self) -> bool {
        *self == "sprintf"
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
            Function::Clear => {
                let is_map = ctx.constant(Some(Map {
                    key: None,
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
                return err!(
                    "function {} expected {} inputs but got {}",
                    self,
                    a,
                    incoming.len()
                );
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
            Binop(Concat) => (smallvec![Str; 2], Str),
            SubstrIndex | SubstrLastIndex | Binop(IsMatch) => (smallvec![Str; 2], Int),
            // Not doesn't unconditionally convert to integers before negating it. Nonempty strings
            // are considered "truthy". Floating point numbers are converted beforehand:
            //    !5 == !1 == 0
            //    !0 == 1
            //    !"hi" == 0
            //    !(0.25) == 1
            Unop(Not) => match &incoming[0] {
                Float | Int => (smallvec![Int], Int),
                Str => (smallvec![Str], Int),
                _ => return err!("unexpected input to Not: {:?}", incoming),
            },
            Binop(LT) | Binop(GT) | Binop(LTE) | Binop(GTE) | Binop(EQ) => (
                match (incoming[0], incoming[1]) {
                    (Str, Str) => smallvec![Str; 2],
                    (Int, Int) | (Null, Int) | (Int, Null) | (Null, Null) => smallvec![Int; 2],
                    (_, Str) | (Str, _) | (Float, _) | (_, Float) => smallvec![Float; 2],
                    _ => return err!("invalid input spec for comparison op: {:?}", incoming),
                },
                Int,
            ),
            LastPart => (smallvec![Str, Str], Str),
            Min | Max => (smallvec![Str,Str,Str], Str),
            StrCmp => (smallvec![Str,Str], Int),
            DefaultIfEmpty => (smallvec![Str,Str], Str),
            AppendIfMissing | PrependIfMissing | RemoveIfEnd | RemoveIfBegin => (smallvec![Str,Str], Str),
            Quote | DoubleQuote => (smallvec![Str], Str),
            Seq => (smallvec![Float,Float,Float], MapIntFloat),
            Uniq => (smallvec![MapIntStr, Str], MapIntStr),
            Binop(Plus) | Binop(Minus) | Binop(Mod) | Binop(Mult) => {
                arith_sig(incoming[0], incoming[1])
            }
            Binop(Pow) | Binop(Div) => (smallvec![Float;2], Float),
            Contains => match incoming[0] {
                MapIntInt | MapIntStr | MapIntFloat => (smallvec![incoming[0], Int], Int),
                MapStrInt | MapStrStr | MapStrFloat => (smallvec![incoming[0], Str], Int),
                _ => return err!("invalid input spec for Contains: {:?}", incoming),
            },
            Delete => match incoming[0] {
                MapIntInt | MapIntStr | MapIntFloat => (smallvec![incoming[0], Int], Int),
                MapStrInt | MapStrStr | MapStrFloat => (smallvec![incoming[0], Str], Int),
                _ => return err!("invalid input spec for Delete: {:?}", incoming),
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
            Clear => {
                if incoming.len() == 1 && incoming[0].is_array() {
                    (smallvec![incoming[0]], Int)
                } else {
                    return err!("invalid input spec for delete (of a map): {:?}", incoming);
                }
            }
            Srand => (smallvec![Int], Int),
            System | HexToInt => (smallvec![Str], Int),
            System2 => (smallvec![Str], MapStrStr),
            ReseedRng => (smallvec![], Int),
            Rand => (smallvec![], Float),
            ToInt => {
                let inc = incoming[0];
                match inc {
                    Null | Int | Float | Str => (smallvec![inc], Int),
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
            Length => (smallvec![incoming[0]], Int),
            Uuid => (smallvec![Str], Str),
            SnowFlake => (smallvec![Int], Int),
            Ulid | Tsid => (smallvec![], Str),
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome => (smallvec![], Str),
            LocalIp => (smallvec![], Str),
            Systime => (smallvec![], Int),
            Strftime => (smallvec![Str, Int], Str),
            Mktime => (smallvec![Str, Int], Int),
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
            SendMail => (smallvec![Str, Str, Str, Str], Null),
            SmtpSend => (smallvec![Str, Str, Str, Str, Str], Null),
            S3Get => (smallvec![Str, Str], Str),
            S3Put => (smallvec![Str, Str, Str], Str),
            KvGet => (smallvec![Str, Str ], Str),
            KvPut => (smallvec![Str, Str,Str], Null),
            KvDelete => (smallvec![Str, Str], Null),
            KvClear => (smallvec![Str], Null),
            LogDebug | LogInfo | LogWarn | LogError => (smallvec![Str], Null),
            SqliteQuery | LibsqlQuery | MysqlQuery | PgQuery => (smallvec![Str, Str], MapIntStr),
            SqliteExecute | LibsqlExecute | MysqlExecute | PgExecute => (smallvec![Str, Str], Int),
            Publish => (smallvec![Str, Str], Null),
            FromJson => (smallvec![Str], MapStrStr),
            ToJson => (smallvec![incoming[0]], Str),
            JsonValue => (smallvec![Str,Str], Str),
            JsonQuery => (smallvec![Str,Str], MapIntStr),
            HtmlValue => (smallvec![Str,Str], Str),
            HtmlQuery => (smallvec![Str,Str], MapIntStr),
            XmlValue => (smallvec![Str,Str], Str),
            XmlQuery => (smallvec![Str,Str], MapIntStr),
            VarDump => (smallvec![incoming[0]], Null),
            ReadAll => (smallvec![Str], Str),
            WriteAll => (smallvec![Str, Str], Null),
            ReadConfig => (smallvec![Str], MapStrStr),
            FromCsv => (smallvec![Str], MapIntStr),
            ToCsv => (smallvec![incoming[0]], Str),
            Trim => (smallvec![Str, Str], Str),
            Truncate => (smallvec![Str, Int, Str], Str),
            Strtonum => (smallvec![Str], Float),
            FormatBytes => (smallvec![Int], Str),
            ToBytes => (smallvec![Str], Int),
            StartsWith => (smallvec![Str, Str], Int),
            EndsWith => (smallvec![Str, Str], Int),
            TextContains => (smallvec![Str, Str], Int),
            Capitalize | UnCapitalize | CamelCase | KebabCase | SnakeCase | TitleCase | Figlet => (smallvec![Str], Str),
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
            Asort => (smallvec![incoming[0],incoming[0]], Int),
            BloomFilterInsert => (smallvec![Str, Str], Null),
            BloomFilterContains | BloomFilterContainsWithInsert => (smallvec![Str, Str], Int),
            Fake => (smallvec![Str, Str], Str),
            TypeOfVariable => (smallvec![incoming[0]], Str),
            IsArray => (smallvec![incoming[0]], Int),
            IsInt => (smallvec![incoming[0]], Int),
            IsNum => (smallvec![incoming[0]], Int),
            IsFormat => (smallvec![Str, Str], Int),
            IntMapJoin => (smallvec![incoming[0], Str], Str),
            ArrayMax | ArrayMin | ArraySum | ArrayMean => {
                if let MapIntInt = incoming[0] {
                    (smallvec![incoming[0]], Int)
                } else if let MapIntFloat = incoming[0] {
                    (smallvec![incoming[0]], Float)
                } else {
                    return err!("invalid input spec for array _max/_min: {:?}", incoming);
                }
            }
            Close => (smallvec![Str], Str),
            Sub | GSub => (smallvec![Str, Str, Str], Int),
            GenSub => (smallvec![Str, Str, Str, Str], Str),
            ToUpper | ToLower | EscapeCSV | EscapeTSV => (smallvec![Str], Str),
            Substr => (smallvec![Str, Int, Int], Str),
            CharAt => (smallvec![Str, Int], Str),
            Chars => (smallvec![Str], MapIntStr),
            Match => (smallvec![Str, Str], Int),
            Exit => (smallvec![Int], Null),
            // Split's second input can be a map of either type
            Split => {
                if let MapIntStr | MapStrStr = incoming[1] {
                    (smallvec![Str, incoming[1], Str], Int)
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
        Some(match self {
            FloatFunc(ff) => ff.arity(),
            IntFunc(bw) => bw.arity(),
            UpdateUsedFields | Rand | Ulid | Tsid | LocalIp | Systime | ReseedRng | ReadErrStdin | NextlineStdin | NextFile
            | ReadLineStdinFused => 0,
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome => 0,
            Exit | ToUpper | ToLower | Clear | Srand | System | System2 | HexToInt | ToInt | EscapeCSV
            | EscapeTSV | Close | Length | ReadErr | ReadErrCmd | Nextline | NextlineCmd
            | Uuid | SnowFlake | Fend | Url | SemVer | Path | DataUrl | DateTime | Shlex | Tuple | Variant | Flags | ParseArray | Func | ToJson | FromJson | ToCsv | FromCsv | TypeOfVariable | IsArray | Unop(_) => 1,
            SetFI | SubstrIndex | SubstrLastIndex | Match | Setcol | Binop(_) => 2,
            JoinCSV | JoinTSV | Delete | Contains => 2,
            Eval => 2,
            DefaultIfEmpty => 2,
            JsonValue | JsonQuery | HtmlValue | HtmlQuery | XmlValue | XmlQuery => 2,
            AppendIfMissing | PrependIfMissing | RemoveIfEnd | RemoveIfBegin => 2,
            Pairs => 3,
            LastPart => 2,
            Hex2Rgb => 1,
            Rgb2Hex => 3,
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
            Encrypt | Decrypt => 3,
            Strftime | Mktime => 2,
            Duration => 1,
            StrCmp => 2,
            CharAt => 2,
            Chars => 1,
            MkBool => 1,
            MkPassword => 1,
            Trim => 2,
            Capitalize | UnCapitalize | Mask | Strtonum | CamelCase | KebabCase | SnakeCase | TitleCase | Words | Lines => 1,
            Figlet => 1,
            Repeat => 2,
            Min | Max => 3,
            Seq => 3,
            Uniq => 2,
            Asort => 2,
            HttpGet => 2,
            HttpPost => 3,
            SendMail => 4,
            SmtpSend => 5,
            S3Get => 2,
            S3Put => 3,
            KvGet | KvDelete => 2,
            KvPut => 3,
            KvClear => 1,
            SqliteQuery | SqliteExecute | LibsqlQuery | LibsqlExecute | MysqlQuery | MysqlExecute | PgQuery | PgExecute => 2,
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
            Unop(Neg) | Unop(Pos) => match &args[0] {
                Some(Scalar(Some(BaseTy::Str))) | Some(Scalar(Some(BaseTy::Float))) => {
                    Ok(Scalar(BaseTy::Float).abs())
                }
                x => Ok(*x),
            },
            Binop(Plus) | Binop(Minus) | Binop(Mod) | Binop(Mult) => {
                Ok(step_arith(&args[0], &args[1]))
            }
            Min | Max => Ok(Scalar(BaseTy::Str).abs()),
            Rand | Binop(Div) | Binop(Pow) => Ok(Scalar(BaseTy::Float).abs()),
            Setcol => Ok(Scalar(BaseTy::Null).abs()),
            Clear | SubstrIndex | SubstrLastIndex | Srand | ReseedRng | Unop(Not) | Binop(IsMatch) | Binop(LT)
            | Binop(GT) | Binop(LTE) | Binop(GTE) | Binop(EQ) | Length | Split | ReadErr
            | ReadErrCmd | ReadErrStdin | Contains | Delete | Match | Sub | GSub | ToInt | Systime | Mktime | Duration
            | System | HexToInt | Asort | MkBool | SnowFlake => Ok(Scalar(BaseTy::Int).abs()),
            System2 => Ok(Map {
                key: BaseTy::Str,
                val: BaseTy::Str,
            }.abs()),
            ToUpper | ToLower | JoinCSV | JoinTSV | Uuid | Ulid | Tsid | LocalIp | Strftime | Fend | Trim | Truncate | JoinCols
            | EscapeCSV | EscapeTSV | Escape
            | Unop(Column) | Binop(Concat) | Nextline | NextlineCmd | NextlineStdin | GenSub | Substr | CharAt
            | Encode | Decode | Digest | Hmac | Jwt | ToJson | JsonValue | HtmlValue | XmlValue | ToCsv | TypeOfVariable | IntMapJoin => {
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
            Whoami | Version | Os | OsFamily | Arch | Pwd | UserHome => {
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
            StartsWith | EndsWith | TextContains => {
                Ok(Scalar(BaseTy::Int).abs())
            }
            BloomFilterInsert => Ok(None),
            BloomFilterContains | BloomFilterContainsWithInsert => {
                Ok(Scalar(BaseTy::Int).abs())
            }
            Strtonum => Ok(Scalar(BaseTy::Float).abs()),
            Capitalize | UnCapitalize | Mask | CamelCase | KebabCase | SnakeCase | TitleCase | Figlet | Repeat => Ok(Scalar(BaseTy::Str).abs()),
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
            SqliteQuery | MysqlQuery | LibsqlQuery | PgQuery  => {
                Ok(Map {
                    key: BaseTy::Int,
                    val: BaseTy::Str,
                }.abs())
            }
            SqliteExecute | LibsqlExecute | MysqlExecute | PgExecute => Ok(Scalar(BaseTy::Int).abs()),
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
                     }) => {
                    Ok(Scalar(BaseTy::Int).abs())
                }
                Some(Map {
                         key: Some(BaseTy::Int),
                         val: Some(BaseTy::Float)
                     }) => {
                    Ok(Scalar(BaseTy::Float).abs())
                }
                _ => { Ok(Scalar(BaseTy::Float).abs()) }
            },
            StrCmp => Ok(Scalar(BaseTy::Int).abs()),
            IncMap => Ok(step_arith(&types::val_of(&args[0])?, &args[2])),
            Exit | SetFI | UpdateUsedFields | NextFile | ReadLineStdinFused | Close => Ok(None),
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
#[allow(clippy::upper_case_acronyms)]
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
}

impl From<Variable> for compile::Ty {
    fn from(v: Variable) -> compile::Ty {
        use Variable::*;
        match v {
            FS | OFS | ORS | RS | FILENAME | CONVFMT => compile::Ty::Str,
            PID | ARGC | NF | NR | FNR | RSTART | RLENGTH => compile::Ty::Int,
            ARGV => compile::Ty::MapIntStr,
            FI => compile::Ty::MapStrInt,
            ENVIRON => compile::Ty::MapStrStr,
            PROCINFO => compile::Ty::MapStrStr,
        }
    }
}

pub(crate) struct Variables<'a> {
    pub argc: Int,
    pub argv: IntMap<Str<'a>>,
    pub fs: Str<'a>,
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
    pub environ: StrMap<'a, Str<'a>>,
    pub procinfo: StrMap<'a, Str<'a>>,
    pub convfmt: Str<'a>,
}

impl<'a> Default for Variables<'a> {
    fn default() -> Variables<'a> {
        Variables {
            argc: 0,
            argv: Default::default(),
            fs: " ".into(),
            ofs: " ".into(),
            ors: "\n".into(),
            rs: "\n".into(),
            nr: 0,
            fnr: 0,
            nf: 0,
            filename: Default::default(),
            convfmt: "%.6g".into(),
            rstart: 0,
            pid: 0,
            rlength: -1,
            fi: Default::default(),
            environ: load_env_variables(),
            procinfo: load_procinfo_variables(),
        }
    }
}

fn load_env_variables<'a>() -> StrMap<'a, Str<'a>> {
    let env = StrMap::default();
    for (k, v) in std::env::vars() {
        env.insert(k.into(), v.into());
    }
    env
}

#[cfg(target_family = "unix")]
fn load_procinfo_variables<'a>() -> StrMap<'a, Str<'a>> {
    let procinfo = StrMap::default();
    procinfo.insert("version".into(), VERSION.into());
    procinfo.insert("strftime".into(), "%a %m %e %H:%M:%S %Z %Y".into());
    procinfo.insert("pid".into(), std::process::id().to_string().into());
    procinfo.insert("platform".into(), "posix".into());
    unsafe {
        procinfo.insert("uid".into(), libc::getuid().to_string().into());
        procinfo.insert("gid".into(), libc::getgid().to_string().into());
        procinfo.insert("euid".into(), libc::geteuid().to_string().into());
        procinfo.insert("egid".into(), libc::getegid().to_string().into());
        procinfo.insert("pgrpid".into(), libc::getpgrp().to_string().into());
        procinfo.insert("ppid".into(), libc::getppid().to_string().into());
    }
    procinfo
}

#[cfg(target_family = "windows")]
fn load_procinfo_variables<'a>() -> StrMap<'a, Str<'a>> {
    let procinfo = StrMap::default();
    procinfo.insert("version".into(), VERSION.into());
    procinfo.insert("strftime".into(), "%a %m %e %H:%M:%S %Z %Y".into());
    procinfo.insert("pid".into(), std::process::id().to_string().into());
    procinfo.insert("platform".into(), "windows".into());
    procinfo
}

impl<'a> Variables<'a> {
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
            FI | ORS | OFS | FS | RS | FILENAME | CONVFMT | ARGV | ENVIRON | PROCINFO => return err!("var {} not an int", var),
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
            FI | ORS | OFS | FS | RS | FILENAME | CONVFMT | ARGV | ENVIRON | PROCINFO => return err!("var {} not an int", var),
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
            FI | PID | ARGC | ARGV | NF | NR | FNR | RSTART | RLENGTH | ENVIRON | PROCINFO => {
                return err!("var {} not a string", var);
            }
        })
    }

    pub fn store_str(&mut self, var: Variable, s: Str<'a>) -> Result<()> {
        use Variable::*;
        match var {
            FS => self.fs = s,
            OFS => self.ofs = s,
            ORS => self.ors = s,
            RS => self.rs = s,
            FILENAME => self.filename = s,
            CONVFMT => self.convfmt = s,
            FI | PID | ARGC | ARGV | NF | NR | FNR | RSTART | RLENGTH | ENVIRON | PROCINFO => {
                return err!("var {} not a string", var);
            }
        };
        Ok(())
    }

    pub fn load_intmap(&self, var: Variable) -> Result<IntMap<Str<'a>>> {
        use Variable::*;
        match var {
            ARGV => Ok(self.argv.clone()),
            FI | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | RLENGTH | ENVIRON | PROCINFO => {
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
            FI | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | RLENGTH | ENVIRON | PROCINFO => {
                err!("var {} is not an int-keyed map", var)
            }
        }
    }

    pub fn load_strmap(&self, var: Variable) -> Result<StrMap<'a, Int>> {
        use Variable::*;
        match var {
            FI => Ok(self.fi.clone()),
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | ENVIRON | PROCINFO
            | RLENGTH => {
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
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | ENVIRON | PROCINFO
            | RLENGTH => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }

    pub fn load_strstrmap(&self, var: Variable) -> Result<StrMap<'a, Str<'a>>> {
        use Variable::*;
        match var {
            ENVIRON => Ok(self.environ.clone()),
            PROCINFO => Ok(self.environ.clone()),
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | FI
            | RLENGTH => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }

    pub fn store_strstrmap(&mut self, var: Variable, m: StrMap<'a, Str<'a>>) -> Result<()> {
        use Variable::*;
        match var {
            ENVIRON => {
                self.environ = m;
                Ok(())
            }
            PROCINFO => {
                self.procinfo = m;
                Ok(())
            }
            ARGV | PID | ORS | OFS | ARGC | NF | NR | FNR | FS | RS | FILENAME | CONVFMT | RSTART | FI
            | RLENGTH => {
                err!("var {} is not a string-keyed map", var)
            }
        }
    }
}

impl Variable {
    pub(crate) fn ty(&self) -> types::TVar<types::BaseTy> {
        use Variable::*;
        match self {
            PID | ARGC | NF | FNR | NR | RSTART | RLENGTH => {
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
            ENVIRON | PROCINFO => types::TVar::Map {
                key: types::BaseTy::Str,
                val: types::BaseTy::Str,
            },
            ORS | OFS | FS | RS | FILENAME | CONVFMT => types::TVar::Scalar(types::BaseTy::Str),
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
    ["PROCINFO", Variable::PROCINFO]
);
