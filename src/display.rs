//! Noisey `Display` impls.
use crate::ast::{Binop, Unop};
use crate::builtins::{Function, Variable};
use crate::cfg::{BasicBlock, Ident, PrimExpr, PrimStmt, PrimVal, Transition};
use crate::common::FileSpec;
use crate::lexer;
use std::fmt::{self, Display, Formatter};
use std::string::String;

pub(crate) struct Wrap(pub Ident);

impl Display for Wrap {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let Ident { low, sub, .. } = self.0;
        write!(f, "{}-{}", low, sub)
    }
}

impl<'a> Display for Transition<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        match &self.0 {
            Some(v) => write!(f, "{}", v),
            None => write!(f, "else"),
        }
    }
}

impl<'a> Display for BasicBlock<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        for i in &self.q {
            writeln!(f, "{}", i)?;
        }
        Ok(())
    }
}

impl<'a> Display for PrimStmt<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use PrimStmt::*;
        match self {
            AsgnIndex(id, pv, pe) => write!(f, "{}[{}] = {}", Wrap(*id), pv, pe),
            AsgnVar(id, pe) => write!(f, "{} = {}", Wrap(*id), pe),
            SetBuiltin(v, pv) => write!(f, "{} = {}", v, pv),
            Return(v) => write!(f, "return {}", v),
            Printf(fmt, args, out) => {
                write!(f, "printf({}", fmt)?;
                for (i, a) in args.iter().enumerate() {
                    if i == args.len() - 1 {
                        write!(f, "{}", a)?;
                    } else {
                        write!(f, "{}, ", a)?;
                    }
                }
                write!(f, ")")?;
                if let Some((out, ap)) = out {
                    let redirect = match ap {
                        FileSpec::Trunc => ">",
                        FileSpec::Append => ">>",
                        FileSpec::Cmd => "|",
                    };
                    write!(f, " {} {}", out, redirect)?;
                }
                Ok(())
            }
            PrintAll(args, out) => {
                write!(f, "print(")?;
                for (i, a) in args.iter().enumerate() {
                    if i == args.len() - 1 {
                        write!(f, "{}", a)?;
                    } else {
                        write!(f, "{}, ", a)?;
                    }
                }
                write!(f, ")")?;
                if let Some((out, ap)) = out {
                    let redirect = match ap {
                        FileSpec::Trunc => ">",
                        FileSpec::Append => ">>",
                        FileSpec::Cmd => "|",
                    };
                    write!(f, " {} {}", out, redirect)?;
                }
                Ok(())
            }
            IterDrop(v) => write!(f, "drop_iter {}", v),
        }
    }
}

impl<'a> Display for PrimExpr<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use PrimExpr::*;
        fn write_func(
            f: &mut Formatter,
            func: impl fmt::Display,
            os: &[impl fmt::Display],
        ) -> fmt::Result {
            write!(f, "{}(", func)?;
            for (i, o) in os.iter().enumerate() {
                let is_last = i == os.len() - 1;
                if is_last {
                    write!(f, "{}", o)?;
                } else {
                    write!(f, "{}, ", o)?;
                }
            }
            write!(f, ")")
        }
        match self {
            Val(v) => write!(f, "{}", v),
            Phi(preds) => {
                write!(f, "phi [")?;
                for (i, (pred, id)) in preds.iter().enumerate() {
                    let is_last = i == preds.len() - 1;
                    if is_last {
                        write!(f, "←{}:{}", pred.index(), Wrap(*id))?
                    } else {
                        write!(f, "←{}:{}, ", pred.index(), Wrap(*id))?
                    }
                }
                write!(f, "]")
            }
            CallBuiltin(b, os) => write_func(f, b, &os[..]),
            CallUDF(func, os) => write_func(f, func, &os[..]),
            Sprintf(fmt, os) => write_func(f, format!("sprintf[{}]", fmt), &os[..]),
            Index(m, v) => write!(f, "{}[{}]", m, v),
            IterBegin(m) => write!(f, "begin({})", m),
            HasNext(i) => write!(f, "hasnext({})", i),
            Next(i) => write!(f, "next({})", i),
            LoadBuiltin(b) => write!(f, "{}", b),
        }
    }
}

impl<'a> Display for PrimVal<'a> {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use PrimVal::*;
        match self {
            Var(id) => write!(f, "{}", Wrap(*id)),
            ILit(n) => write!(f, "{}@int", *n),
            FLit(n) => write!(f, "{}@float", *n),
            StrLit(s) => write!(f, "\"{}\"", String::from_utf8_lossy(s)),
        }
    }
}

impl Display for Function {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use Function::*;
        match self {
            Unop(u) => write!(f, "{}", u),
            Binop(b) => write!(f, "{}", b),
            FloatFunc(ff) => write!(f, "{}", ff.func_name()),
            IntFunc(bw) => write!(f, "{}", bw.func_name()),
            ReadErr => write!(f, "hasline"),
            ReadErrCmd => write!(f, "hasline(cmd)"),
            Nextline => write!(f, "nextline"),
            NextlineCmd => write!(f, "nextline(cmd)"),
            ReadErrStdin => write!(f, "hasline(stdin)"),
            NextlineStdin => write!(f, "nextline(stdin)"),
            ReadLineStdinFused => write!(f, "stdin-fused"),
            NextFile => write!(f, "nextfile"),
            Setcol => write!(f, "$="),
            Split => write!(f, "split"),
            Length => write!(f, "length"),
            Strlen => write!(f, "strlen"),
            Uuid => write!(f, "uuid"),
            Ulid => write!(f, "ulid"),
            Tsid => write!(f, "tsid"),
            SnowFlake => write!(f, "snowflake"),
            LocalIp => write!(f, "local_ip"),
            Whoami => write!(f, "whoami"),
            Version => write!(f, "version"),
            Os => write!(f, "os"),
            OsFamily => write!(f, "os_family"),
            Arch => write!(f, "arch"),
            Pwd => write!(f, "pwd"),
            UserHome => write!(f, "user_home"),
            GetEnv => write!(f, "getenv"),
            Systime => write!(f, "systime"),
            Strftime => write!(f, "strftime"),
            Mktime => write!(f, "mktime"),
            Duration => write!(f, "duration"),
            MkBool => write!(f, "mkbool"),
            MkPassword => write!(f, "mkpass"),
            Fend => write!(f, "fend"),
            Eval => write!(f, "eval"),
            Trim => write!(f, "trim"),
            Truncate => write!(f, "truncate"),
            Parse => write!(f, "parse"),
            RegexParse => write!(f, "rparse"),
            Strtonum => write!(f, "strtonum"),
            FormatBytes => write!(f, "format_bytes"),
            ToBytes => write!(f, "to_bytes"),
            StartsWith => write!(f, "starts_with"),
            EndsWith => write!(f, "ends_with"),
            TextContains => write!(f, "contains"),
            Capitalize => write!(f, "capitalize"),
            UnCapitalize => write!(f, "uncapitalize"),
            CamelCase => write!(f, "camel_case"),
            KebabCase => write!(f, "kebab_case"),
            SnakeCase => write!(f, "snake_case"),
            TitleCase => write!(f, "title_case"),
            Figlet => write!(f, "figlet"),
            PadLeft => write!(f, "pad_left"),
            PadRight => write!(f, "pad_right"),
            PadBoth => write!(f, "pad_both"),
            StrCmp => write!(f, "strcmp"),
            Mask => write!(f, "mask"),
            Repeat => write!(f, "repeat"),
            DefaultIfEmpty => write!(f, "default_if_empty"),
            AppendIfMissing => write!(f, "append_if_missing"),
            PrependIfMissing => write!(f, "prepend_if_missing"),
            RemoveIfEnd => write!(f, "remove_if_end"),
            RemoveIfBegin => write!(f, "remove_if_begin"),
            Quote => write!(f, "quote"),
            DoubleQuote => write!(f, "double_quote"),
            Words => write!(f, "words"),
            Lines => write!(f, "lines"),
            Escape => write!(f, "escape"),
            Encode => write!(f, "encode"),
            Decode => write!(f, "decode"),
            Digest => write!(f, "digest"),
            Hmac => write!(f, "hmac"),
            Jwt => write!(f, "jwt"),
            Dejwt => write!(f, "dejwt"),
            Encrypt => write!(f, "encrypt"),
            Decrypt => write!(f, "decrypt"),
            Url => write!(f, "url"),
            Pairs => write!(f, "pairs"),
            Record => write!(f, "record"),
            Message => write!(f, "message"),
            SemVer => write!(f, "semver"),
            Path => write!(f, "path"),
            DataUrl => write!(f, "data_url"),
            DateTime => write!(f, "datetime"),
            Shlex => write!(f, "shlex"),
            Tuple => write!(f, "tuple"),
            Flags => write!(f, "flags"),
            ParseArray => write!(f, "parse_array"),
            Hex2Rgb => write!(f, "hex2rgb"),
            Rgb2Hex => write!(f, "rgb2hex"),
            Variant => write!(f, "variant"),
            Func => write!(f, "func"),
            HttpGet => write!(f, "http_get"),
            HttpPost => write!(f, "http_post"),
            SendMail => write!(f, "send_mail"),
            SmtpSend => write!(f, "smtp_send"),
            S3Get => write!(f, "s3_get"),
            S3Put => write!(f, "s3_put"),
            KvGet => write!(f, "kv_get"),
            KvPut => write!(f, "kv_put"),
            KvDelete => write!(f, "kv_delete"),
            KvClear => write!(f, "kv_clear"),
            LogDebug => write!(f, "log_debug"),
            LogInfo => write!(f, "log_info"),
            LogWarn => write!(f, "log_warn"),
            LogError => write!(f, "log_error"),
            SqliteQuery => write!(f, "sqlite_query"),
            SqliteExecute => write!(f, "sqlite_execute"),
            LibsqlQuery => write!(f, "libsql_query"),
            LibsqlExecute => write!(f, "libsql_execute"),
            MysqlQuery => write!(f, "mysql_query"),
            MysqlExecute => write!(f, "mysql_execute"),
            PgQuery => write!(f, "pg_query"),
            PgExecute => write!(f, "pg_execute"),
            Publish => write!(f, "publish"),
            FromJson => write!(f, "from_json"),
            ToJson => write!(f, "to_json"),
            JsonValue => write!(f, "json_value"),
            JsonQuery => write!(f, "json_query"),
            HtmlValue => write!(f, "html_value"),
            HtmlQuery => write!(f, "html_query"),
            XmlValue => write!(f, "xml_value"),
            XmlQuery => write!(f, "xml_query"),
            VarDump => write!(f, "var_dump"),
            ReadAll => write!(f, "read_all"),
            WriteAll => write!(f, "write_all"),
            ReadConfig => write!(f, "read_config"),
            FromCsv => write!(f, "from_csv"),
            ToCsv => write!(f, "to_csv"),
            Min => write!(f, "min"),
            Max => write!(f, "max"),
            ArrayMax => write!(f, "_max"),
            ArrayMin => write!(f, "_min"),
            ArraySum => write!(f, "_sum"),
            ArrayMean => write!(f, "_mean"),
            Seq => write!(f, "seq"),
            IntMapJoin => write!(f, "_join"),
            Asort => write!(f, "asort"),
            BloomFilterInsert => write!(f, "bf_insert"),
            BloomFilterContains => write!(f, "bf_contains"),
            BloomFilterContainsWithInsert => write!(f, "bf_icontains"),
            Fake => write!(f, "fake"),
            TypeOfVariable => write!(f, "typeof"),
            IsArray => write!(f, "isarray"),
            IsInt => write!(f, "isint"),
            IsNum => write!(f, "isnum"),
            IsFormat => write!(f, "is"),
            Uniq => write!(f, "uniq"),
            Contains => write!(f, "contains"),
            Delete => write!(f, "delete"),
            Clear => write!(f, "clear"),
            Close => write!(f, "close"),
            Match => write!(f, "match"),
            SubstrIndex => write!(f, "index"),
            SubstrLastIndex => write!(f, "last_index"),
            LastPart => write!(f, "last_part"),
            Sub => write!(f, "sub"),
            GSub => write!(f, "gsub"),
            GenSub => write!(f, "gensub"),
            EscapeCSV => write!(f, "escape_csv"),
            EscapeTSV => write!(f, "escape_tsv"),
            JoinCSV => write!(f, "join_csv"),
            JoinTSV => write!(f, "join_tsv"),
            JoinCols => write!(f, "join_fields"),
            Substr => write!(f, "substr"),
            CharAt => write!(f, "char_at"),
            Chars => write!(f, "chars"),
            ToInt => write!(f, "int"),
            HexToInt => write!(f, "hex"),
            Rand => write!(f, "rand"),
            Srand => write!(f, "srand"),
            ReseedRng => write!(f, "srand_reseed"),
            System => write!(f, "system"),
            System2 => write!(f, "system2"),
            UpdateUsedFields => write!(f, "update_used_fields"),
            SetFI => write!(f, "set-FI"),
            ToLower => write!(f, "tolower"),
            ToUpper => write!(f, "toupper"),
            IncMap => write!(f, "inc_map"),
            Exit => write!(f, "exit"),
        }
    }
}

impl Display for Variable {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use Variable::*;
        write!(
            f,
            "{}",
            match self {
                ARGC => "ARGC",
                ARGV => "ARGV",
                OFS => "OFS",
                ORS => "ORS",
                FS => "FS",
                RS => "RS",
                NF => "NF",
                NR => "NR",
                FNR => "FNR",
                FILENAME => "FILENAME",
                RSTART => "RSTART",
                RLENGTH => "RLENGTH",
                PID => "PID",
                FI => "FI",
                ENVIRON => "ENVIRON",
                PROCINFO => "PROCINFO",
                CONVFMT => "CONVFMT",
            }
        )
    }
}

impl Display for Unop {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use Unop::*;
        write!(
            f,
            "{}",
            match self {
                Column => "$",
                Not => "!",
                Neg => "-",
                Pos => "+",
            }
        )
    }
}

impl Display for Binop {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        use Binop::*;
        write!(
            f,
            "{}",
            match self {
                Plus => "+",
                Minus => "-",
                Mult => "*",
                Div => "/",
                Mod => "%",
                Concat => "<concat>",
                IsMatch => "~",
                Pow => "^",
                LT => "<",
                GT => ">",
                LTE => "<=",
                GTE => ">=",
                EQ => "==",
            }
        )
    }
}

impl Display for lexer::Loc {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        write!(fmt, "line {}, column {}", self.line + 1, self.col + 1)
    }
}

impl Display for lexer::Error {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        write!(fmt, "{}. {}", self.location, self.desc)
    }
}

impl<'a> Display for lexer::Tok<'a> {
    fn fmt(&self, fmt: &mut Formatter) -> fmt::Result {
        use lexer::Tok::*;
        let rep = match self {
            Begin => "BEGIN",
            Prepare => "PREPARE",
            BeginFile => "BEGINFILE",
            EndFile => "ENDFILE",
            End => "END",
            Break => "break",
            Continue => "continue",
            Next => "next",
            NextFile => "nextfile",
            For => "for",
            If => "if",
            Else => "else",
            Exit => "exit",
            ExitLP => "exit(",
            Print => "print",
            Printf => "printf",
            // Separate token for a "print(" and "printf(".
            PrintLP => "print(",
            PrintfLP => "printf(",
            While => "while",
            Do => "do",

            // { }
            LBrace => "{",
            RBrace => "}",
            // [ ]
            LBrack => "[",
            RBrack => "]",
            // ( )
            LParen => "(",
            RParen => ")",

            Getline => "getline",
            Pipe => "|",
            Assign => "=",
            Add => "+",
            AddAssign => "+=",
            Sub => "-",
            SubAssign => "-=",
            Mul => "*",
            MulAssign => "*=",
            Div => "/",
            DivAssign => "/=",
            Pow => "^",
            PowAssign => "^=",
            Mod => "%",
            ModAssign => "%=",
            Match => "~",
            NotMatch => "!~",

            EQ => "==",
            NEQ => "!=",
            LT => "<",
            GT => ">",
            LTE => "<=",
            GTE => ">=",
            Incr => "++",
            Decr => "--",
            Not => "!",

            AND => "&&",
            OR => "||",
            QUESTION => "?",
            COLON => ":",

            Append => ">>",

            Dollar => "$",
            Semi => ";",
            Newline => "\\n",
            Comma => ",",
            In => "in",
            Delete => "delete",
            Return => "return",

            Ident(s) => return write!(fmt, "identifier({})", s),
            StrLit(s) => return write!(fmt, "{:?}", s),
            PatLit(s) => return write!(fmt, "/{}/", s),
            CallStart(s) => return write!(fmt, "{}(", s),
            FunDec(s) => return write!(fmt, "function {}", s),

            ILit(s) | HexLit(s) | FLit(s) => return write!(fmt, "{}", s),
        };
        write!(fmt, "{}", rep)
    }
}
