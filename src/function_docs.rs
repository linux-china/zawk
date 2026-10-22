//! Reference documentation of the builtin functions, used by `zawk functions [--json]`.
//!
//! Every public builtin (the keys of `builtins::FUNCTIONS`) must have an entry here: this is
//! checked by the tests below, which also check that `info/stdlib.md` mentions every function.
//!
//! A signature is written as `name(p1: type, p2: type[, p3: type[, p4: type]]) -> type`: the
//! parameters after the first `[` are optional. Types are `str`, `int`, `float`, `num`, `bool`
//! (an int `0` or `1`), `any`, `array` (any array) or `map[K]V` (an array with K keys and V
//! values), and `void` for a function without a meaningful return value.

use serde_json::{json, Value};

pub(crate) struct FunctionDoc {
    pub name: &'static str,
    pub category: &'static str,
    pub signature: &'static str,
    pub summary: &'static str,
    pub example: &'static str,
}

pub(crate) struct Param {
    pub name: String,
    pub ty: String,
    pub optional: bool,
}

const fn d(
    name: &'static str,
    category: &'static str,
    signature: &'static str,
    summary: &'static str,
    example: &'static str,
) -> FunctionDoc {
    FunctionDoc { name, category, signature, summary, example }
}

const TEXT: &str = "Text";
const ESCAPE: &str = "Escape";
const PARSER: &str = "Parser";
const ID: &str = "ID";
const ARRAY: &str = "Array";
const MATH: &str = "Math";
const BITWISE: &str = "Bitwise";
const DATETIME: &str = "Date/Time";
const COLOR: &str = "Color";
const FAKE: &str = "Fake";
const DECISION: &str = "Decision";
const JSON: &str = "JSON";
const CSV: &str = "CSV";
const XML: &str = "XML";
const HTML: &str = "HTML";
const ENCODING: &str = "Encoding";
const CRYPTO: &str = "Crypto";
const KV: &str = "KV";
const NETWORK: &str = "Network";
const DATABASE: &str = "Database";
const OS: &str = "OS";
const IO: &str = "I/O";
const MISC: &str = "Misc";

pub(crate) static FUNCTION_DOCS: &[FunctionDoc] = &[
    // Text
    d("length", TEXT, "length([x: any]) -> int",
      "Number of characters of x (default $0), or number of elements of an array.",
      r#"length("你好")"#),
    d("strlen", TEXT, "strlen(s: str) -> int", "Byte length of s.", r#"strlen("你好")"#),
    d("char_at", TEXT, "char_at(s: str, i: int) -> str",
      "Character at 1-based index i, empty if out of range.", r#"char_at("abc", 2)"#),
    d("chars", TEXT, "chars(s: str) -> map[int]str", "Characters of s as an array, starting from 1.",
      r#"chars("abc")"#),
    d("match", TEXT, "match(s: str, re: str[, arr: array]) -> int",
      "Position of the first match of re in s (0 if none); sets RSTART/RLENGTH, and the groups into arr.",
      r#"match("foo123", /[0-9]+/)"#),
    d("substr", TEXT, "substr(s: str, i: int[, n: int]) -> str",
      "Substring of s starting at 1-based character i, n characters or to the end.",
      r#"substr("hello", 2, 3)"#),
    d("sub", TEXT, "sub(re: str, repl: str[, target: str]) -> int",
      "Replace the first match of re in target (default $0) in place; returns the number of replacements.",
      r#"sub(/o/, "0", s)"#),
    d("gsub", TEXT, "gsub(re: str, repl: str[, target: str]) -> int",
      "Replace all matches of re in target (default $0) in place; returns the number of replacements.",
      r#"gsub(/o/, "0", s)"#),
    d("gensub", TEXT, "gensub(re: str, repl: str, how: str[, target: str]) -> str",
      "gawk gensub: return target (default $0) with the how-th (or \"g\" all) matches replaced, \\1..\\9 refer to groups.",
      r#"gensub(/(a)(b)/, "\\2\\1", "g", "abab")"#),
    d("index", TEXT, "index(s: str, t: str) -> int",
      "1-based character position of the first occurrence of t in s, 0 if absent.", r#"index("abcabc", "b")"#),
    d("last_index", TEXT, "last_index(s: str, t: str) -> int",
      "1-based character position of the last occurrence of t in s, 0 if absent.",
      r#"last_index("abcabc", "b")"#),
    d("split", TEXT, "split(s: str, arr: array[, fs: str[, seps: array]]) -> int",
      "Split s by fs (default FS) into arr, and the separators into seps (seps[i] follows arr[i]); returns the number of elements.", r#"split("a,b,c", arr, ",")"#),
    d("patsplit", TEXT, "patsplit(s: str, arr: array, re: str[, seps: array]) -> int",
      "Split s into the matches of re (arr) and the text between them (seps[0] precedes arr[1], seps[i] follows arr[i]); returns the number of matches.",
      r#"patsplit("a1b22c", arr, /[0-9]+/)"#),
    d("last_part", TEXT, "last_part(s: str[, sep: str]) -> str",
      "Last part of s split by sep; without sep, `/` is tried and then `.`.", r#"last_part("a/b/c")"#),
    d("sprintf", TEXT, "sprintf(fmt: str, args: any...) -> str", "Format args according to the printf format fmt.",
      r#"sprintf("%05.2f", 3.14159)"#),
    d("hex", TEXT, "hex(s: str) -> int", "Parse a hexadecimal number such as 0xFF or ff (64 bits, wrapping), 0 otherwise.", r#"hex("0xFF")"#),
    d("join_fields", TEXT, "join_fields(i: int, j: int[, sep: str]) -> str",
      "Join columns i..j (inclusive) with sep (default OFS).", r#"join_fields(2, 4, "-")"#),
    d("join_csv", TEXT, "join_csv(i: int, j: int) -> str", "Join columns i..j as a CSV row.",
      "join_csv(1, 3)"),
    d("join_tsv", TEXT, "join_tsv(i: int, j: int) -> str", "Join columns i..j as a TSV row.",
      "join_tsv(1, 3)"),
    d("tolower", TEXT, "tolower(s: str) -> str", "Lowercase s.", r#"tolower("HeLLo")"#),
    d("toupper", TEXT, "toupper(s: str) -> str", "Uppercase s.", r#"toupper("HeLLo")"#),
    d("trim", TEXT, "trim(s: str[, chars: str]) -> str",
      "Remove whitespace, or any of chars, from both ends of s.", r#"trim("[hello]", "[]")"#),
    d("truncate", TEXT, "truncate(s: str, len: int[, suffix: str]) -> str",
      "Truncate s to len characters, ending with suffix (default \"...\") when cut.",
      r#"truncate("hello world", 8)"#),
    d("capitalize", TEXT, "capitalize(s: str) -> str", "Uppercase the first character.", r#"capitalize("hello")"#),
    d("uncapitalize", TEXT, "uncapitalize(s: str) -> str", "Lowercase the first character.",
      r#"uncapitalize("Hello")"#),
    d("camel_case", TEXT, "camel_case(s: str) -> str", "Convert to camelCase.", r#"camel_case("hello world")"#),
    d("kebab_case", TEXT, "kebab_case(s: str) -> str", "Convert to kebab-case.", r#"kebab_case("hello world")"#),
    d("snake_case", TEXT, "snake_case(s: str) -> str", "Convert to snake_case.", r#"snake_case("hello world")"#),
    d("title_case", TEXT, "title_case(s: str) -> str", "Convert to Title Case.", r#"title_case("hello world")"#),
    d("pascal_case", TEXT, "pascal_case(s: str) -> str", "Alias of title_case.", r#"pascal_case("hello world")"#),
    d("isint", TEXT, "isint(x: any) -> bool", "1 if x is an integer (or integer text).", r#"isint("123")"#),
    d("isnum", TEXT, "isnum(x: any) -> bool", "1 if x is a number (or numeric text).", r#"isnum("1234.01")"#),
    d("is", TEXT, "is(format: str, s: str) -> bool",
      "Validate s against a format: email, url, phone, ip.", r#"is("email", "demo@example.com")"#),
    d("starts_with", TEXT, "starts_with(s: str, prefix: str) -> bool", "1 if s starts with prefix.",
      r#"starts_with("https://a.com", "https://")"#),
    d("ends_with", TEXT, "ends_with(s: str, suffix: str) -> bool", "1 if s ends with suffix.",
      r#"ends_with("a.com", ".com")"#),
    d("contains", TEXT, "contains(s: str, t: str) -> bool", "1 if s contains t.",
      r#"contains("https://a.com", "//")"#),
    d("mask", TEXT, "mask(s: str) -> str", "Mask the middle of sensitive text (email, phone, ...) with `*`.",
      r#"mask("abc@example.com")"#),
    d("pad", TEXT, "pad(s: str, len: int[, fill: str]) -> str",
      "Pad both sides of s to len characters with fill (default space).", r#"pad("hi", 6, "*")"#),
    d("pad_start", TEXT, "pad_start(s: str, len: int[, fill: str]) -> str",
      "Pad the start of s to len characters with fill (default space).", r#"pad_start("hi", 5, "*")"#),
    d("pad_end", TEXT, "pad_end(s: str, len: int[, fill: str]) -> str",
      "Pad the end of s to len characters with fill (default space).", r#"pad_end("hi", 5, "*")"#),
    d("strcmp", TEXT, "strcmp(a: str, b: str) -> int", "Compare texts: -1, 0 or 1.", r#"strcmp("a", "b")"#),
    d("lines", TEXT, "lines(s: str) -> map[int]str", "Non-empty lines of s.", r#"lines("a\nb")"#),
    d("words", TEXT, "words(s: str) -> map[int]str", "Words of s (CJK characters are separate words).",
      r#"words("hello world? 你好")"#),
    d("repeat", TEXT, "repeat(s: str, n: int) -> str", "Repeat s n times.", r#"repeat("*", 3)"#),
    d("default_if_empty", TEXT, "default_if_empty(s: str, default: str) -> str",
      "default if s is empty or blank, s otherwise.", r#"default_if_empty("  ", "demo")"#),
    d("append_if_missing", TEXT, "append_if_missing(s: str, suffix: str) -> str",
      "Append suffix if s does not end with it.", r#"append_if_missing("example.com", "/")"#),
    d("prepend_if_missing", TEXT, "prepend_if_missing(s: str, prefix: str) -> str",
      "Prepend prefix if s does not start with it.", r#"prepend_if_missing("example.com", "https://")"#),
    d("remove_if_end", TEXT, "remove_if_end(s: str, suffix: str) -> str", "Remove suffix if s ends with it.",
      r#"remove_if_end("demo.json", ".json")"#),
    d("remove_if_begin", TEXT, "remove_if_begin(s: str, prefix: str) -> str",
      "Remove prefix if s starts with it.", r#"remove_if_begin("file://demo.json", "file://")"#),
    d("quote", TEXT, "quote(s: str) -> str", "Shell-quote s: wrap in single quotes, inner ' becomes '\\''.",
      r#"quote("it's")"#),
    d("double_quote", TEXT, "double_quote(s: str) -> str",
      "Wrap s in double quotes, escaping inner \" and \\ with a backslash.", r#"double_quote("say \"hi\"")"#),
    d("parse", TEXT, "parse(s: str, pattern: str) -> map[str]str",
      "Extract named `{name}` placeholders of a wildcard pattern.",
      r#"parse("Hello World", "{greet} {name}")["name"]"#),
    d("rparse", TEXT, "rparse(s: str, re: str) -> map[int]str", "Extract the groups of a regex match.",
      r#"rparse("Hello World", "(\\w+) (\\w+)")[2]"#),
    d("format_bytes", TEXT, "format_bytes(n: int) -> str", "Human readable size of n bytes.",
      "format_bytes(1024)"),
    d("to_bytes", TEXT, "to_bytes(s: str) -> int", "Parse a human readable size (KB, MiB, ...) into bytes.",
      r#"to_bytes("2 KB")"#),
    d("mkpass", TEXT, "mkpass([len: int]) -> str",
      "Random password of len (default 8) characters with digits, letters and symbols.", "mkpass(12)"),
    d("figlet", TEXT, "figlet(s: str) -> str", "ASCII art banner of s (ASCII characters only).",
      r#"figlet("zawk")"#),
    // Escape
    d("escape", ESCAPE, "escape(format: str, s: str) -> str",
      "Escape s for a format: json, csv, tsv, xml, html, sql, shell.", r#"escape("json", s)"#),
    d("escape_csv", ESCAPE, "escape_csv(s: str) -> str", "Escape s as a CSV column.", r#"escape_csv("a,b")"#),
    d("escape_tsv", ESCAPE, "escape_tsv(s: str) -> str", "Escape s as a TSV column.", r#"escape_tsv("a\tb")"#),
    // Parser
    d("url", PARSER, "url(s: str) -> map[str]str",
      "Parse a URL: schema, user, password, host, port, path, query, fragment.",
      r#"url("https://example.com/user/1")["host"]"#),
    d("data_url", PARSER, "data_url(s: str) -> map[str]str", "Parse a data URL: data, mime_type, encoding.",
      r#"data_url("data:text/plain;base64,SGVsbG8=")["data"]"#),
    d("shlex", PARSER, "shlex(s: str) -> map[int]str", "Split a command line like a POSIX shell.",
      r#"shlex("ls -l 'my dir'")"#),
    d("path", PARSER, "path(s: str) -> map[str]str",
      "File path info: exists, full_path, parent, file_name, file_stem, file_ext, content_type.",
      r#"path("./demo.txt")["file_ext"]"#),
    d("semver", PARSER, "semver(s: str) -> map[str]str", "Parse a semantic version: major, minor, patch, pre, build.",
      r#"semver("1.2.3-alpha")["minor"]"#),
    d("pairs", PARSER, "pairs(s: str[, sep: str[, kv_sep: str]]) -> map[str]str",
      "Parse key/value pairs (default `,` and `=`); with sep `&` values are URL decoded.",
      r#"pairs("id=1&name=Hello%20World", "&")["name"]"#),
    d("record", PARSER, "record(s: str) -> map[str]str", "Parse a Prometheus/OpenMetrics style record.",
      r#"record("http_requests_total{method='post',code=200}")"#),
    d("message", PARSER, "message(s: str) -> map[str]str", "Parse a record with a body: name, headers and body.",
      r#"message("login_event{method='post'}('a@example.com')")"#),
    d("func", PARSER, "func(s: str) -> map[int]str", "Parse a function invocation: [0] is the name, [1..] the arguments.",
      r#"func("hello(1,2,3)")[0]"#),
    d("tuple", PARSER, "tuple(s: str) -> map[int]str", "Parse a tuple literal.", r#"tuple("(1,2,'first')")"#),
    d("variant", PARSER, "variant(s: str) -> map[str]str", "Parse a variant such as `week(5)`.",
      r#"variant("week(5)")"#),
    d("parse_array", PARSER, "parse_array(s: str) -> map[int]str", "Parse an array literal.",
      r#"parse_array("['first','second']")"#),
    d("flags", PARSER, "flags(s: str) -> map[str]int", "Parse a flag set such as `{vip,top20}`.",
      r#"flags("{vip,top20}")["vip"]"#),
    // ID
    d("uuid", ID, "uuid([version: str]) -> str", "UUID, version v4 (default) or v7.", r#"uuid("v7")"#),
    d("ulid", ID, "ulid() -> str", "ULID: Universally Unique Lexicographically Sortable Identifier.", "ulid()"),
    d("tsid", ID, "tsid() -> str", "TSID: time-sorted ID.", "tsid()"),
    d("snowflake", ID, "snowflake([machine_id: int]) -> int",
      "64-bit Snowflake ID; machine_id defaults to 1, max 65535.", "snowflake(2)"),
    // Array
    d("seq", ARRAY, "seq(a: num[, b: num[, c: num]]) -> map[int]float",
      "Number sequence like the seq command: seq(end) from 0, seq(start, end), seq(start, step, end).",
      "seq(1, 2, 9)"),
    d("uniq", ARRAY, "uniq(arr: map[int]str) -> map[int]str", "Sorted unique values of arr.",
      r#"split("b a b", arr); u = uniq(arr)"#),
    d("asort", ARRAY, "asort(src: array[, dest: array[, how: str]]) -> int",
      "Sort the values of src into dest (default in place) indexed from 1, in the order how (a predefined PROCINFO[\"sorted_in\"] order such as \"@val_num_desc\"; default: numbers before strings, ascending); returns the length.",
      r#"split("b c a", arr); n = asort(arr, out, "@val_str_desc")"#),
    d("asorti", ARRAY, "asorti(src: array[, dest: array[, how: str]]) -> int",
      "Sort the indices of src into dest (default in place) indexed from 1, in the order how (default \"@ind_str_asc\"); returns the length.",
      r#"a["b"]; a["a"]; n = asorti(a, keys)"#),
    d("_max", ARRAY, "_max(arr: array) -> num", "Max of the (numeric) values of an array; 0 if empty.", "arr[1] = 3; arr[2] = 5; m = _max(arr)"),
    d("_min", ARRAY, "_min(arr: array) -> num", "Min of the (numeric) values of an array; 0 if empty.", "arr[1] = 3; arr[2] = 5; m = _min(arr)"),
    d("_sum", ARRAY, "_sum(arr: array) -> num", "Sum of the (numeric) values of an array.", "arr[1] = 3; arr[2] = 5; m = _sum(arr)"),
    d("_mean", ARRAY, "_mean(arr: array) -> num", "Mean of the (numeric) values of an array; 0 if empty.", "arr[1] = 3; arr[2] = 5; m = _mean(arr)"),
    d("_median", ARRAY, "_median(arr: array) -> float", "Median of the (numeric) values of an array; 0 if empty.",
      "arr[1] = 3; arr[2] = 1; arr[3] = 4; m = _median(arr)"),
    d("_quantile", ARRAY, "_quantile(arr: array, q: float) -> float",
      "q-quantile (0..1) of the values of an array, interpolated linearly between ranks; 0 if empty.",
      "lat[1] = 120; lat[2] = 80; lat[3] = 300; p99 = _quantile(lat, 0.99)"),
    d("_var", ARRAY, "_var(arr: array) -> float", "Sample variance (n - 1) of the values of an array; 0 for fewer than 2 values.",
      "arr[1] = 2; arr[2] = 4; v = _var(arr)"),
    d("_stddev", ARRAY, "_stddev(arr: array) -> float", "Sample standard deviation (n - 1) of the values of an array.",
      "arr[1] = 2; arr[2] = 4; s = _stddev(arr)"),
    d("_mode", ARRAY, "_mode(arr: array) -> float", "Most frequent (numeric) value of an array, the smallest on a tie; 0 if empty.",
      "arr[1] = 3; arr[2] = 3; arr[3] = 1; m = _mode(arr)"),
    d("_count_distinct", ARRAY, "_count_distinct(arr: array) -> int", "Number of distinct values of an array.",
      r#"split("a b a", ip); n = _count_distinct(ip)"#),
    d("_top", ARRAY, "_top(arr: array, k: int) -> map[int]str",
      "The k keys with the largest numeric values, from the largest down, indexed from 1 (ties by key).",
      r#"c["a"] = 3; c["b"] = 5; t = _top(c, 1)"#),
    d("_join", ARRAY, "_join(arr: map[int]any[, sep: str]) -> str", "Join array values with sep (default space).",
      r#"split("a b", arr); s = _join(arr, ",")"#),
    d("bf_insert", ARRAY, "bf_insert(item: str[, group: str]) -> void", "Insert item into a bloom filter.",
      "bf_insert($1)"),
    d("bf_contains", ARRAY, "bf_contains(item: str[, group: str]) -> bool",
      "1 if item may be in the bloom filter.", "bf_contains($1)"),
    d("bf_icontains", ARRAY, "bf_icontains(item: str[, group: str]) -> bool",
      "Like bf_contains, and insert item if absent: handy for duplicate checks.", "!bf_icontains($1)"),
    // Math
    d("rand", MATH, "rand() -> float", "Uniform random number in [0, 1).", "rand()"),
    d("srand", MATH, "srand([seed: int]) -> int", "Seed the random generator (default: time); returns the old seed.",
      "srand(42)"),
    d("int", MATH, "int(x: any) -> int", "Truncate x toward zero.", r#"int("11.7")"#),
    d("float", MATH, "float(s: str) -> float", "Parse a number, decimal or with a 0x prefix.", r#"float("0x11")"#),
    d("strtonum", MATH, "strtonum(s: str) -> float",
      "gawk strtonum: 0x is hexadecimal and a leading 0 octal.", r#"strtonum("0x11")"#),
    d("abs", MATH, "abs(x: num) -> float", "Absolute value.", "abs(-1)"),
    d("ceil", MATH, "ceil(x: num) -> float", "Round up.", "ceil(4.5)"),
    d("floor", MATH, "floor(x: num) -> float", "Round down.", "floor(4.5)"),
    d("round", MATH, "round(x: num) -> float", "Round to nearest.", "round(4.4)"),
    d("exp", MATH, "exp(x: num) -> float", "e raised to x.", "exp(1)"),
    d("log", MATH, "log(x: num) -> float", "Natural logarithm.", "log(10)"),
    d("log2", MATH, "log2(x: num) -> float", "Base 2 logarithm.", "log2(8)"),
    d("log10", MATH, "log10(x: num) -> float", "Base 10 logarithm.", "log10(100)"),
    d("sqrt", MATH, "sqrt(x: num) -> float", "Square root.", "sqrt(2)"),
    d("sin", MATH, "sin(x: num) -> float", "Sine (radians).", "sin(0)"),
    d("cos", MATH, "cos(x: num) -> float", "Cosine (radians).", "cos(0)"),
    d("atan", MATH, "atan(x: num) -> float", "Arc tangent.", "atan(1)"),
    d("atan2", MATH, "atan2(y: num, x: num) -> float", "Arc tangent of y/x.", "atan2(0, -1)"),
    d("intdiv0", MATH, "intdiv0(x: num, y: num) -> int",
      "Integer division truncated toward zero; 0 when y is 0.", "intdiv0(-7, 3)"),
    d("min", MATH, "min(a: any, b: any[, c: any]) -> any", "Smallest value (numbers or texts).", "min(1, 2, 3)"),
    d("max", MATH, "max(a: any, b: any[, c: any]) -> any", "Largest value (numbers or texts).", r#"max("A", "B")"#),
    d("eval", MATH, "eval(formula: str[, context: map[str]str]) -> float",
      "Evaluate an arithmetic/boolean expression, with variables from context.", r#"eval("a + 2", ctx)"#),
    d("fend", MATH, "fend(expr: str) -> str", "Evaluate with the fend calculator (units supported).",
      r#"fend("1+2")"#),
    d("mkbool", MATH, "mkbool(s: str) -> bool", "Parse a boolean: true/yes/y/1/✓ are 1.", r#"mkbool("Yes")"#),
    // Bitwise
    d("and", BITWISE, "and(a: int, b: int) -> int", "Bitwise and.", "and(6, 3)"),
    d("or", BITWISE, "or(a: int, b: int) -> int", "Bitwise or.", "or(6, 3)"),
    d("xor", BITWISE, "xor(a: int, b: int) -> int", "Bitwise xor.", "xor(5, 3)"),
    d("compl", BITWISE, "compl(a: int) -> int", "Bitwise complement of the 64-bit two's complement value (compl(0) is -1).", "compl(0)"),
    d("lshift", BITWISE, "lshift(a: int, n: int) -> int", "Shift left (64-bit); counts outside 0..63 give 0.", "lshift(1, 4)"),
    d("rshift", BITWISE, "rshift(a: int, n: int) -> int", "Arithmetic shift right; counts outside 0..63 give 0 or -1 (sign fill).", "rshift(-16, 2)"),
    d("rshiftl", BITWISE, "rshiftl(a: int, n: int) -> int", "Logical shift right; counts outside 0..63 give 0.", "rshiftl(-1, 60)"),
    // Date/Time
    d("systime", DATETIME, "systime() -> int", "Current Unix time in seconds.", "systime()"),
    d("strftime", DATETIME, "strftime([format: str[, timestamp: int[, utc: bool]]]) -> str",
      "Format a Unix time (default now) with chrono strftime format, local time unless utc.",
      r#"strftime("%Y-%m-%d %H:%M:%S", 0, 1)"#),
    d("mktime", DATETIME, "mktime(s: str[, utc: bool[, tz_offset: int]]) -> int",
      "Parse date time text to a Unix time, -1 if invalid; tz_offset in hours.",
      r#"mktime("2024-01-01 10:00:00", 0, 8)"#),
    d("datetime", DATETIME, "datetime([time: any]) -> map[str]int",
      "Date/time parts (year, month, monthday, hour, minute, second, yearday, weekday) of a timestamp or text, default now.",
      r#"datetime("2020-02-02")["year"]"#),
    d("duration", DATETIME, "duration(s: str) -> int", "Parse a duration expression into seconds.",
      r#"duration("2min + 12sec")"#),
    // Color
    d("hex2rgb", COLOR, "hex2rgb(s: str) -> map[int]int", "Hex color to array [r, g, b].", r##"hex2rgb("#FF0000")"##),
    d("rgb2hex", COLOR, "rgb2hex(r: int[, g: int[, b: int]]) -> str", "RGB to hex color.", "rgb2hex(255, 0, 0)"),
    // Fake
    d("fake", FAKE, "fake(kind: str[, locale: str]) -> str",
      "Fake data: name, phone, cell, email, wechat, ip, creditcard, zipcode, plate, postcode, id; locale EN or CN.",
      r#"fake("name", "cn")"#),
    // Decision
    d("jev", DECISION, "jev(input: any, question: str[, threshold: float]) -> bool",
      "Yes/no decision by the TypeSafe API (TYPESAFE_API_KEY); threshold defaults to jev_threshold or 0.5.",
      r#"jev($0, "the name is European")"#),
    d("jev_prob", DECISION, "jev_prob(input: any, question: str) -> float", "Probability of yes, 0 to 1.",
      r#"jev_prob($0, "the customer is angry")"#),
    d("jev_choice", DECISION, "jev_choice(input: any, question: str, options: any) -> str",
      "Pick one of options (array or comma separated text).",
      r#"jev_choice($0, "which team should handle this?", "billing,tech")"#),
    d("jev_score", DECISION, "jev_score(input: any, question: str, levels: any) -> float",
      "Probability-weighted position on the levels, 0 to length(levels) - 1.",
      r#"jev_score($0, "how luxurious is this?", "budget,mid,premium")"#),
    // JSON
    d("from_json", JSON, "from_json(s: str) -> map[str]str", "Parse JSON text into an array.",
      r#"from_json("{\"id\":1}")["id"]"#),
    d("to_json", JSON, "to_json(x: any) -> str", "Encode a value or array as JSON.", "to_json(arr)"),
    d("json_value", JSON, "json_value(json: str, path: str) -> str", "Single value selected by a JSONPath.",
      r#"json_value(s, "$.store.book[0].title")"#),
    d("json_query", JSON, "json_query(json: str, path: str) -> map[int]str", "Values selected by a JSONPath.",
      r#"json_query(s, "$.store.book[*].title")"#),
    d("json_has", JSON, "json_has(json: str, key: str) -> bool", "1 if the JSON object has the key.",
      r#"json_has(s, "id")"#),
    d("json_get", JSON, "json_get(json: str, key: str) -> str", "Value of a key of the JSON object, \"\" if missing.",
      r#"json_get(s, "name")"#),
    d("json_set", JSON, "json_set(json: str, key: str, value: str) -> str",
      "Set a key of the JSON object; valid JSON value text is kept as JSON, other text becomes a string.",
      r#"json_set(s, "age", 18)"#),
    d("json_del", JSON, "json_del(json: str, key: str) -> str", "Remove a key from the JSON object.",
      r#"json_del(s, "password")"#),
    // CSV
    d("from_csv", CSV, "from_csv(s: str) -> map[int]str", "Parse a CSV row.", r#"from_csv("a,\"b,c\"")"#),
    d("to_csv", CSV, "to_csv(arr: array) -> str", "Encode array values as a CSV row.",
      r#"split("a b", arr); s = to_csv(arr)"#),
    // XML
    d("xml_value", XML, "xml_value(xml: str, xpath: str) -> str", "Inner text of the node selected by an XPath.",
      r#"xml_value(s, "/book/title")"#),
    d("xml_query", XML, "xml_query(xml: str, xpath: str) -> map[int]str", "String values of nodes selected by an XPath.",
      r#"xml_query(s, "//title")"#),
    // HTML
    d("html_value", HTML, "html_value(html: str, selector: str) -> str",
      "Inner text of the first node matching a CSS selector.", r#"html_value(s, "head > title")"#),
    d("html_query", HTML, "html_query(html: str, selector: str) -> map[int]str",
      "Inner texts of nodes matching a CSS selector.", r#"html_query(s, "ul > li")"#),
    // Encoding
    d("encode", ENCODING, "encode(format: str, s: str) -> str",
      "Encode: hex, base32, base58, base62, base64, base64url, zlib2base64url, url, hex-base64, base64-hex, ...",
      r#"encode("base64", "hi")"#),
    d("decode", ENCODING, "decode(format: str, s: str) -> str", "Decode, same formats as encode.",
      r#"decode("base64", "aGk=")"#),
    // Crypto
    d("digest", CRYPTO, "digest(algorithm: str, s: str) -> str",
      "Hash: md5, sha256, sha512, bcrypt, murmur3, xxh32, xxh64, blake3, crc32, adler32.",
      r#"digest("sha256", "hello")"#),
    d("hash", CRYPTO, "hash(algorithm: str, s: str) -> str", "Alias of digest.", r#"hash("md5", "hello")"#),
    d("hmac", CRYPTO, "hmac(algorithm: str, key: str, s: str) -> str", "HMAC signature: HmacSHA256, HmacSHA512.",
      r#"hmac("HmacSHA256", "secret", "hello")"#),
    d("encrypt", CRYPTO, "encrypt(mode: str, plaintext: str, key: str) -> str",
      "Encrypt with aes-128-gcm, aes-256-gcm (recommended), aes-128-cbc or aes-256-cbc; output `v3:` + base64(salt + nonce + ciphertext), CBC `v4:` with an HMAC-SHA256 tag.",
      r#"encrypt("aes-256-gcm", "Secret Text", "pass")"#),
    d("decrypt", CRYPTO, "decrypt(mode: str, ciphertext: str, key: str) -> str",
      "Decrypt the output of encrypt, empty on failure.", r#"decrypt("aes-256-gcm", s, "pass")"#),
    d("jwt", CRYPTO, "jwt(algorithm: str, key: str, payload: map[str]str) -> str",
      "Sign a JWT: HS256/384/512 with a secret, RS*/ES*/EdDSA with a PEM private key.",
      r#"payload["sub"] = "1"; token = jwt("HS256", "secret", payload)"#),
    d("dejwt", CRYPTO, "dejwt(key: str, token: str) -> map[str]str",
      "Verify a JWT with a secret, PEM public key or JWKS URL; exp/nbf checked when present. Returns the payload, empty if invalid (ERRNO tells why).",
      r#"dejwt("secret", token)["sub"]"#),
    // KV
    d("kv_get", KV, "kv_get(namespace: str, key: str) -> str",
      "Get a value; namespace is a SQLite db name, redis://, rediss:// or nats:// URL.", r#"kv_get("ns1", "nick")"#),
    d("kv_put", KV, "kv_put(namespace: str, key: str, value: str) -> void", "Put a value.",
      r#"kv_put("ns1", "nick", "jackie")"#),
    d("kv_delete", KV, "kv_delete(namespace: str, key: str) -> void", "Delete a key.", r#"kv_delete("ns1", "nick")"#),
    d("kv_clear", KV, "kv_clear(namespace: str) -> void", "Delete all keys of a namespace.", r#"kv_clear("ns1")"#),
    // Network
    d("http_get", NETWORK, "http_get(url: str[, headers: map[str]str]) -> map[str]str",
      "HTTP GET; response has status (0 on network error), text and header names.",
      r#"http_get("https://httpbin.org/get")["status"]"#),
    d("http_post", NETWORK, "http_post(url: str[, body: str[, headers: map[str]str]]) -> map[str]str",
      "HTTP POST; a JSON body sets Content-Type: application/json.",
      r#"http_post("https://httpbin.org/post", "{\"id\":1}")["text"]"#),
    d("send_mail", NETWORK, "send_mail(from: str, to: str, subject: str, body: str[, smtp_url: str]) -> void",
      "Send email by SMTP (smtp_url or SMTP_URL, smtps://user:password@host:465), MailerSend (MLSN_API_KEY) or Resend (RESEND_API_KEY); to is comma separated.",
      r#"send_mail("a@example.com", "b@example.com", "Hi", "Hello", "smtp://localhost:1025")"#),
    d("send_email", NETWORK, "send_email(from: str, to: str, subject: str, body: str[, smtp_url: str]) -> void",
      "Deprecated alias of send_mail.", r#"send_email("a@example.com", "b@example.com", "Hi", "Hello")"#),
    d("smtp_send", NETWORK, "smtp_send(smtp_url: str, from: str, to: str, subject: str, body: str) -> void",
      "Deprecated: use send_mail(from, to, subject, body, smtp_url).",
      r#"smtp_send("smtp://localhost:1025", "a@example.com", "b@example.com", "Hi", "Hello")"#),
    d("s3_get", NETWORK, "s3_get(bucket: str, object: str) -> str", "Get an S3 object as text (S3_* or AWS_* env).",
      r#"s3_get("bucket1", "demo.txt")"#),
    d("s3_put", NETWORK, "s3_put(bucket: str, object: str, body: str) -> str", "Put text as an S3 object.",
      r#"s3_put("bucket1", "demo.txt", "hello")"#),
    d("publish", NETWORK, "publish(url: str, body: str) -> void", "Publish a message to NATS or MQTT: nats://host:4222/topic.",
      r#"publish("nats://localhost:4222/topic", "hello")"#),
    d("local_ip", NETWORK, "local_ip() -> str", "Local IP address.", "local_ip()"),
    // Database
    d("sqlite_query", DATABASE, "sqlite_query(db: str, sql: str[, params: map[int]str]) -> map[int]str", "Query SQLite; one row per element; params bind `?` in order.",
      r#"sqlite_query("sqlite.db", "select nick, email from user")"#),
    d("sqlite_execute", DATABASE, "sqlite_execute(db: str, sql: str[, params: map[int]str]) -> int", "Execute SQL on SQLite; returns affected rows.",
      r#"sqlite_execute("sqlite.db", "delete from user where nick = ?", arr)"#),
    d("pg_query", DATABASE, "pg_query(url: str, sql: str[, params: map[int]str]) -> map[int]str", "Query PostgreSQL.",
      r#"pg_query("postgresql://postgres:postgres@localhost/db", "select id from people")"#),
    d("pg_execute", DATABASE, "pg_execute(url: str, sql: str[, params: map[int]str]) -> int", "Execute SQL on PostgreSQL.",
      r#"pg_execute("postgresql://postgres:postgres@localhost/db", "delete from people")"#),
    d("mysql_query", DATABASE, "mysql_query(url: str, sql: str[, params: map[int]str]) -> map[int]str", "Query MySQL.",
      r#"mysql_query("mysql://root:123456@localhost:3306/test", "select id from people")"#),
    d("mysql_execute", DATABASE, "mysql_execute(url: str, sql: str[, params: map[int]str]) -> int", "Execute SQL on MySQL.",
      r#"mysql_execute("mysql://root:123456@localhost:3306/test", "delete from people")"#),
    // OS
    d("whoami", OS, "whoami() -> str", "Current user name.", "whoami()"),
    d("os", OS, "os() -> str", "Operating system, such as linux or macos.", "os()"),
    d("os_family", OS, "os_family() -> str", "OS family, such as unix or windows.", "os_family()"),
    d("arch", OS, "arch() -> str", "CPU architecture, such as x86_64 or aarch64.", "arch()"),
    d("pwd", OS, "pwd() -> str", "Current working directory.", "pwd()"),
    d("user_home", OS, "user_home() -> str", "Home directory of the user.", "user_home()"),
    d("getenv", OS, "getenv(name: str[, default: str]) -> str", "Environment variable (`.env` is loaded) or default.",
      r#"getenv("HOME", "/tmp")"#),
    d("system", OS, "system(cmd: str) -> int", "Run a shell command; returns its exit status.", r#"system("ls")"#),
    d("system2", OS, "system2(cmd: str) -> map[str]str", "Run a shell command; returns code, stdout and stderr.",
      r#"system2("echo hi")["stdout"]"#),
    // I/O
    d("read_all", IO, "read_all(path: str) -> str", "Read a local file or http(s) URL as text.",
      r#"read_all("demo.txt")"#),
    d("write_all", IO, "write_all(path: str, s: str) -> void", "Write text to a file, replacing it.",
      r#"write_all("demo.txt", "hello")"#),
    d("read_config", IO, "read_config(path: str) -> map[str]str", "Read an .ini or .properties file.",
      r#"read_config("demo.ini")"#),
    d("close", IO, "close(name: str) -> int", "Close a file, command or pipe opened by redirection or getline.",
      r#"close("sort")"#),
    d("fflush", IO, "fflush([name: str]) -> int", "Flush an output file or pipe, or all output.", "fflush()"),
    // Misc
    d("typeof", MISC, "typeof(x: any) -> str", "gawk type name: number, string, strnum, array, untyped, ...",
      "typeof(x)"),
    d("isarray", MISC, "isarray(x: any) -> bool", "1 if x is an array.", "isarray(x)"),
    d("var_dump", MISC, "var_dump(x: any) -> void", "Dump a value or array to stderr.", "var_dump(arr)"),
    d("pprint", MISC, "pprint(x: any) -> void", "Alias of var_dump.", "pprint(arr)"),
    d("log_debug", MISC, "log_debug(msg: str) -> void", "Log at debug level to stderr (RUST_LOG).",
      r#"log_debug("start")"#),
    d("log_info", MISC, "log_info(msg: str) -> void", "Log at info level to stderr.", r#"log_info("start")"#),
    d("log_warn", MISC, "log_warn(msg: str) -> void", "Log at warn level to stderr.", r#"log_warn("slow")"#),
    d("log_error", MISC, "log_error(msg: str) -> void", "Log at error level to stderr.", r#"log_error("failed")"#),
    d("version", MISC, "version() -> str", "zawk version.", "version()"),
    d("assert", MISC, "assert(cond: bool, msg: str) -> void",
      "If cond is false (tested as by if), print msg to stderr and exit with code 2.",
      "assert(NF == 5, f\"line {NR}: expect 5 fields, got {NF}\")"),
    d("exit", MISC, "exit([code: int]) -> void", "Exit with code (default 0), running END actions.", "exit(1)"),
];

impl FunctionDoc {
    pub(crate) fn params(&self) -> Vec<Param> {
        let start = self.signature.find('(').map_or(0, |i| i + 1);
        let end = self.signature.rfind(") -> ").unwrap_or(start).max(start);
        let mut params = vec![];
        let mut push = |param: &mut String, optional: bool| {
            let text = std::mem::take(param);
            let text = text.trim();
            if !text.is_empty() {
                let (name, ty) = text.split_once(':').unwrap_or((text, "any"));
                params.push(Param { name: name.trim().to_string(), ty: ty.trim().to_string(), optional });
            }
        };
        // A `[` makes this and the following parameters optional, except in a `map[K]V` type.
        let mut optional = false;
        let mut param = String::new();
        let mut chars = self.signature[start..end].chars();
        while let Some(c) = chars.next() {
            match c {
                '[' if param.ends_with("map") => {
                    param.push(c);
                    for c in chars.by_ref() {
                        param.push(c);
                        if c == ']' {
                            break;
                        }
                    }
                }
                '[' => {
                    push(&mut param, optional);
                    optional = true;
                }
                ']' => {}
                ',' => push(&mut param, optional),
                _ => param.push(c),
            }
        }
        push(&mut param, optional);
        params
    }

    pub(crate) fn returns(&self) -> &'static str {
        self.signature.rsplit_once("->").map_or("void", |(_, r)| r.trim())
    }

    pub(crate) fn to_json(&self) -> Value {
        let params: Vec<Value> = self
            .params()
            .iter()
            .map(|p| json!({"name": p.name, "type": p.ty, "optional": p.optional}))
            .collect();
        json!({
            "name": self.name,
            "category": self.category,
            "signature": self.signature,
            "params": params,
            "returns": self.returns(),
            "summary": self.summary,
            "example": self.example,
        })
    }
}

/// The minimum and maximum number of arguments of the builtin `name`, from its documented
/// signature; `None` for an undocumented or variadic function.
pub(crate) fn arg_range(name: &str) -> Option<(usize, usize)> {
    let doc = FUNCTION_DOCS.iter().find(|f| f.name == name)?;
    let params = doc.params();
    if params.iter().any(|p| p.ty.ends_with("...")) {
        return None;
    }
    let required = params.iter().filter(|p| !p.optional).count();
    Some((required, params.len()))
}

/// The docs whose name contains `filter`, or whose category is `filter` (case-insensitive).
pub(crate) fn find(filter: Option<&str>) -> Vec<&'static FunctionDoc> {
    FUNCTION_DOCS
        .iter()
        .filter(|f| match filter {
            None => true,
            Some(q) => f.name.contains(q) || f.category.eq_ignore_ascii_case(q),
        })
        .collect()
}

/// NDJSON with one function per line: compact for agents, streamable, and still greppable.
pub(crate) fn render_ndjson(docs: &[&FunctionDoc]) -> String {
    docs.iter().map(|f| format!("{}\n", f.to_json())).collect()
}

pub(crate) fn render_text(docs: &[&FunctionDoc]) -> String {
    let mut out = String::new();
    let mut category = "";
    for f in docs {
        if f.category != category {
            category = f.category;
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("# {}\n", category));
        }
        out.push_str(&format!("{}\n    {}  e.g. {}\n", f.signature, f.summary, f.example));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtins::FUNCTIONS;
    use hashbrown::HashSet;

    /// Builtins that are parsed as syntax rather than looked up in `FUNCTIONS`.
    const SYNTAX_FUNCTIONS: &[&str] = &["sprintf", "assert"];

    #[test]
    fn docs_cover_all_builtins() {
        let documented: HashSet<&str> = FUNCTION_DOCS.iter().map(|f| f.name).collect();
        assert_eq!(documented.len(), FUNCTION_DOCS.len(), "duplicate function docs");
        let mut missing: Vec<_> = FUNCTIONS.keys().filter(|k| !documented.contains(*k)).collect();
        missing.sort();
        assert!(missing.is_empty(), "builtins without docs in function_docs.rs: {:?}", missing);
        let mut unknown: Vec<_> = documented
            .iter()
            .filter(|n| !FUNCTIONS.contains_key(*n) && !SYNTAX_FUNCTIONS.contains(n))
            .collect();
        unknown.sort();
        assert!(unknown.is_empty(), "docs for unknown functions: {:?}", unknown);
    }

    #[test]
    fn signatures_are_well_formed() {
        for f in FUNCTION_DOCS {
            assert!(f.signature.starts_with(&format!("{}(", f.name)), "bad signature: {}", f.signature);
            assert!(f.signature.contains(") -> "), "missing return type: {}", f.signature);
            assert!(f.example.contains(f.name), "example of {} does not call it: {}", f.name, f.example);
            assert!(!f.summary.is_empty(), "missing summary: {}", f.name);
            for p in f.params() {
                assert!(!p.name.is_empty() && !p.ty.is_empty(), "bad param in {}", f.signature);
            }
        }
    }

    #[test]
    fn signature_params() {
        let optional = |name: &str| -> Vec<bool> {
            let f = FUNCTION_DOCS.iter().find(|f| f.name == name).unwrap();
            f.params().iter().map(|p| p.optional).collect()
        };
        assert_eq!(optional("trim"), [false, true]);
        assert_eq!(optional("strftime"), [true, true, true]);
        assert_eq!(optional("pairs"), [false, true, true]);
        assert_eq!(optional("gensub"), [false, false, false, true]);
        assert!(optional("ulid").is_empty());
        let jwt = FUNCTION_DOCS.iter().find(|f| f.name == "jwt").unwrap();
        assert_eq!(jwt.params()[2].ty, "map[str]str");
        assert_eq!(jwt.returns(), "str");
    }

    #[test]
    fn stdlib_md_covers_all_builtins() {
        let stdlib = include_str!("../info/stdlib.md");
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        // `name(` not preceded by an identifier character, so that `for(` is not `or(`
        let called = |name: &str| {
            let call = format!("{}(", name);
            stdlib.match_indices(&call).any(|(i, _)| !stdlib[..i].chars().next_back().is_some_and(is_ident))
        };
        let mentioned = |name: &str| {
            stdlib.contains(&format!("`{}`", name))
                || called(name)
                || stdlib.lines().any(|l| {
                    l.starts_with('#') && l.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == name)
                })
        };
        let mut missing: Vec<_> = FUNCTION_DOCS.iter().map(|f| f.name).filter(|n| !mentioned(n)).collect();
        missing.sort();
        assert!(missing.is_empty(), "functions not documented in info/stdlib.md: {:?}", missing);
    }
}
