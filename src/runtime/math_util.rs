use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use evalexpr::{DefaultNumericTypes, Value};
use lazy_static::lazy_static;
use logos::Logos;
use semver::{Version};
use snowflake::SnowflakeIdGenerator;
use std::hash::Hash;
use crate::runtime::{stdlib_warning, Float, Int, IntMap, SharedMap, Str, StrMap};
use crate::runtime::compare::looks_numeric;

/// Floating point remainder (the result has the sign of `x`, as with C's `fmod`). Integral
/// operands, the common case (`$1 % 100`), use integer division, which is much faster than `fmod`.
#[inline]
pub(crate) fn fmod(x: Float, y: Float) -> Float {
    const LIMIT: Float = 9007199254740992.0; // 2^53
    if x == x.trunc() && y == y.trunc() && x.abs() < LIMIT && y.abs() < LIMIT && y != 0.0 {
        let r = ((x as Int) % (y as Int)) as Float;
        // Keep the sign of a zero result, as fmod does: -4 % 2 is -0.
        return if r == 0.0 { r.copysign(x) } else { r };
    }
    x % y
}

pub fn min(first: &str, second: &str, third: &str) -> String {
    pick(first, second, third, Ordering::Less)
}

pub fn max(first: &str, second: &str, third: &str) -> String {
    pick(first, second, third, Ordering::Greater)
}

/// Pick the min/max item: compare as numbers if all items are numeric, otherwise as strings.
/// An empty `third` means only 2 params; on ties the earliest item wins.
fn pick(first: &str, second: &str, third: &str, wanted: Ordering) -> String {
    let items: &[&str] = if third.is_empty() { &[first, second] } else { &[first, second, third] };
    let nums: Option<Vec<f64>> = items.iter().map(|item| item.parse::<f64>().ok()).collect();
    let mut best = 0;
    for i in 1..items.len() {
        let ordering = match &nums {
            Some(nums) => nums[i].total_cmp(&nums[best]),
            None => items[i].cmp(items[best]),
        };
        if ordering == wanted {
            best = i;
        }
    }
    items[best].to_string()
}

/// A value that `asort()` can sort, in gawk's order: numbers before strings, numbers in numeric
/// order and strings in string order. `strnum` is whether string values may be strnums.
pub(crate) trait AsortValue: Sized {
    fn asort_sort(items: &mut Vec<Self>, strnum: bool);
}

impl AsortValue for Int {
    fn asort_sort(items: &mut Vec<Self>, _strnum: bool) {
        items.sort_unstable();
    }
}

impl AsortValue for Float {
    fn asort_sort(items: &mut Vec<Self>, _strnum: bool) {
        // Equal under total_cmp means bitwise equal, so an unstable sort is fine.
        items.sort_unstable_by(|x, y| x.total_cmp(y));
    }
}

impl<'a> AsortValue for Str<'a> {
    fn asort_sort(items: &mut Vec<Self>, strnum: bool) {
        // Decorate-sort-undecorate: get the bytes (and, for strnums, the numeric value) of each
        // item once, rather than in every comparison, and sort indices into `items`.
        // The pointers stay valid while `items` is neither moved nor modified; `get_bytes` forces
        // Concat strings, so all of them are computed before any is dereferenced.
        let bytes: Vec<*const [u8]> = items.iter().map(|s| s.get_bytes()).collect();
        let bytes = |i: usize| unsafe { &*bytes[i] };
        let mut nums: Vec<(Float, usize)> = Vec::new();
        let mut strs: Vec<usize> = Vec::with_capacity(items.len());
        for i in 0..items.len() {
            // Strings are untyped at runtime, so strings that may be strnums (such as the
            // elements created by split()) sort as numbers when they look numeric.
            match if strnum { looks_numeric(bytes(i)) } else { None } {
                Some(n) => nums.push((n, i)),
                None => strs.push(i),
            }
        }
        // Equal numbers keep their original order (as with a stable sort); equal strings are
        // indistinguishable, so their order does not matter.
        nums.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        strs.sort_unstable_by(|&x, &y| bytes(x).cmp(bytes(y)));
        let mut order: Vec<usize> = nums.iter().map(|&(_, i)| i).collect();
        drop(nums);
        order.extend(strs);
        // Permute in place so that `items[k]` becomes the old `items[order[k]]`, following each
        // cycle and marking the positions it fills as done (`order[j] == j`).
        for i in 0..order.len() {
            let mut j = i;
            while order[j] != i {
                let k = order[j];
                items.swap(j, k);
                order[j] = j;
                j = k;
            }
            order[j] = j;
        }
    }
}

/// `asort(src, dst)`: replaces the contents of `dst` with the values of `src` sorted, indexed from
/// 1, and returns their number. `src` and `dst` may be the same array (`asort(arr)`). `strnum` is
/// whether the string values of `src` may be strnums; if not, they sort as strings.
pub(crate) fn asort<K1, K2, V>(src: &SharedMap<K1, V>, dst: &SharedMap<K2, V>, strnum: bool) -> Int
where
    K1: Hash + Eq,
    K2: Hash + Eq + From<Int>,
    V: AsortValue + Clone,
{
    let mut items: Vec<V> = src.iter(|it| it.map(|(_, v)| v.clone()).collect());
    V::asort_sort(&mut items, strnum);
    dst.clear();
    let len = items.len() as Int;
    for (i, item) in items.into_iter().enumerate() {
        dst.insert(K2::from(i as Int + 1), item);
    }
    len
}

pub(crate) fn map_int_int_join(obj: &IntMap<Int>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.sort();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.join(sep)
}

pub(crate) fn map_int_float_join(obj: &IntMap<Float>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.sort();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.join(sep)
}

pub(crate) fn map_int_str_join(obj: &IntMap<Str>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.sort();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.join(sep)
}

pub(crate) fn map_int_int_max(obj: &IntMap<Int>) -> Int {
    let len = obj.len();
    return if len == 0 {
        0
    } else {
        let mut max = obj.get(&(1i64));
        if len == 1 {
            return max;
        }
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            if item > max {
                max = item;
            }
        }
        max
    };
}

pub(crate) fn map_int_float_max(obj: &IntMap<Float>) -> Float {
    let len = obj.len();
    return if len == 0 {
        0f64
    } else {
        let mut max = obj.get(&(1i64));
        if len == 1 {
            return max;
        }
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            if item > max {
                max = item;
            }
        }
        max
    };
}

pub(crate) fn map_int_int_min(obj: &IntMap<Int>) -> Int {
    let len = obj.len();
    return if len == 0 {
        0
    } else {
        let mut min = obj.get(&(1i64));
        if len == 1 {
            return min;
        }
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            if item < min {
                min = item;
            }
        }
        min
    };
}

pub(crate) fn map_int_float_min(obj: &IntMap<Float>) -> Float {
    let len = obj.len();
    return if len == 0 {
        0f64
    } else {
        let mut min = obj.get(&(1i64));
        if len == 1 {
            return min;
        }
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            if item < min {
                min = item;
            }
        }
        min
    };
}


pub(crate) fn map_int_int_sum(obj: &IntMap<Int>) -> Int {
    let len = obj.len();
    return if len == 0 {
        0
    } else {
        let mut total = 0;
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            total = total + item;
        }
        total
    };
}

pub(crate) fn map_int_float_sum(obj: &IntMap<Float>) -> Float {
    let len = obj.len();
    return if len == 0 {
        0f64
    } else {
        let mut total = 0f64;
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            total = total + item;
        }
        total
    };
}

pub(crate) fn map_int_int_mean(obj: &IntMap<Int>) -> Int {
    let len = obj.len();
    return if len == 0 {
        0
    } else {
        let mut total = 0;
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            total = total + item;
        }
        total / (len as i64)
    };
}

pub(crate) fn map_int_float_mean(obj: &IntMap<Float>) -> Float {
    let len = obj.len();
    return if len == 0 {
        0f64
    } else {
        let mut total = 0.0f64;
        let items = obj.to_vec();
        for index in items {
            let item = obj.get(&index);
            total = total + item;
        }
        total / (len as f64)
    };
}


const NO: &'static [&'static str] = &["false", "no", "𐄂", "0", "0.0", "0.00", "00.0",
    "0x0", "0x00", "0X0", "0X00", "0o0", "0o00", "0O0", "0O00", "0b0", "0b00", "0B0", "0B00"];

pub(crate) fn mkbool(text: &str) -> i64 {
    let text = text.trim().to_lowercase();
    return if text.is_empty() || NO.contains(&text.as_str()) {
        0
    } else {
        1
    };
}

/// The array `[start, start + step, ...]` of the numbers up to `end`; empty if `start > end`. A
/// non-positive step (which would never reach `end`) or more than
/// [`MAX_RESULT_LEN`](crate::runtime::MAX_RESULT_LEN) elements is an error.
pub(crate) fn seq(start: Float, step: Float, end: Float) -> crate::common::Result<IntMap<Float>> {
    if start <= end {
        if !(step > 0.0) {
            return err!("seq: step {} must be positive", step);
        }
        crate::runtime::check_result_len("seq", "element count", ((end - start) / step).floor() + 1.0)?;
    }
    let result: IntMap<Float> = IntMap::default();
    let mut ir = start;
    let mut index = 1;
    while ir <= end {
        result.insert(index, ir);
        ir += step;
        index += 1;
    }
    Ok(result)
}

pub(crate) fn uuid(version: &str) -> String {
    match version {
        "v7" => uuid::Uuid::now_v7().to_string(),
        "v4" | &_ => uuid::Uuid::new_v4().to_string()
    }
}

lazy_static! {
    static ref SNOWFLAKES: Arc<Mutex<HashMap<u16, SnowflakeIdGenerator>>> = Arc::new(Mutex::new(HashMap::new()));
}

///  machine ID(10 bits) should be less 1024
pub(crate) fn snowflake(machine_id: u16) -> Int {
    let mut pool = SNOWFLAKES.lock().unwrap();
    let generator = pool.entry(machine_id).or_insert_with(|| {
        let new_machine_id = (machine_id >> 5) & (32 - 1);
        let new_node_id = machine_id & (32 - 1);
        SnowflakeIdGenerator::new(new_machine_id as i32, new_node_id as i32)
    });
    generator.real_time_generate() as Int
}

pub(crate) fn ulid() -> String {
    ulid::Ulid::generate().to_string()
}

pub(crate) fn tsid() -> String {
    tsid::create_tsid().to_string()
}

/// gawk's `strtonum`. `strnum` is whether the argument may be a strnum (input such as a field):
/// one that looks like a decimal number is that number (`strtonum($1)` is 17 for the field
/// "017"). Otherwise a leading `0x` or `0X` starts a hexadecimal number and a leading `0` an
/// octal one (`strtonum("017")` is 15), unless the digits continue with `8`, `9`, `.` or an
/// exponent, which make it decimal (`"018"` is 18, `"017.5"` is 17.5). Leading blanks or a sign
/// also make it decimal.
pub(crate) fn awk_strtonum(bs: &[u8], strnum: bool) -> Float {
    use crate::runtime::float_parse::strtod;
    if strnum {
        if let Some(n) = crate::runtime::compare::looks_numeric(bs) {
            return n;
        }
    }
    let decimal = |bs: &[u8]| {
        let start = bs.iter().position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        start.map_or(0.0, |i| strtod(&bs[i..]))
    };
    match bs {
        [b'0', b'x' | b'X', rest @ ..] => crate::runtime::float_parse::hex_digits(rest).0,
        [b'0', rest @ ..] => {
            let mut n = 0.0;
            for b in rest {
                match b {
                    b'0'..=b'7' => n = n * 8.0 + (b - b'0') as Float,
                    b'8' | b'9' | b'.' | b'e' | b'E' => return decimal(bs),
                    _ => break,
                }
            }
            n
        }
        _ => decimal(bs),
    }
}

/// Converts a number to the 64-bit two's complement pattern used by the bitwise functions:
/// the value is truncated toward zero and reduced modulo 2^64, so `2^63` becomes the bit pattern
/// `0x8000000000000000` instead of saturating to `i64::MAX`. NaN and infinities become 0.
pub(crate) fn float_to_bits(f: Float) -> Int {
    const TWO_63: Float = 9223372036854775808.0;
    const TWO_64: Float = 18446744073709551616.0;
    if f.abs() < TWO_63 {
        f as Int
    } else {
        f.trunc().rem_euclid(TWO_64) as u64 as Int
    }
}

/// The hexadecimal digits of `text` if it is a `0x` number with an optional sign.
fn hex_digits_of(text: &str) -> Option<&str> {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    unsigned.strip_prefix("0x").or_else(|| unsigned.strip_prefix("0X"))
}

pub(crate) fn is_str_int(text: &str) -> bool {
    let text = text.trim();
    match hex_digits_of(text) {
        Some(h) => h.bytes().all(|b| b.is_ascii_hexdigit()) && i64::from_str_radix(h, 16).is_ok(),
        None => text.parse::<i64>().is_ok(),
    }
}

pub(crate) fn is_str_num(text: &str) -> bool {
    let text = text.trim();
    match hex_digits_of(text) {
        Some(h) => !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()),
        None => text.parse::<f64>().is_ok(),
    }
}

pub(crate) fn uniq<'a>(obj: &IntMap<Str<'a>>, _param: &str) -> IntMap<Str<'a>> {
    //todo uniq implement logic with param
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.sort();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.dedup();
    let result: IntMap<Str> = IntMap::default();
    let mut index: i64 = 1;
    for item in items {
        result.insert(index, Str::from(item));
        index = index + 1;
    }
    result
}

pub(crate) fn shlex<'a>(text: &str) -> IntMap<Str<'a>> {
    let args = shlex::split(text).unwrap_or(vec![]);
    let result: IntMap<Str> = IntMap::default();
    let mut index: i64 = 1;
    for item in args {
        result.insert(index, Str::from(item));
        index = index + 1;
    }
    result
}

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t\n\f]+")] // Ignore this regex pattern between tokens
enum TupleToken<'a> {
    #[token("(")]
    LBRACE,
    #[token(")")]
    RBRACE,
    #[token(",")]
    COMMA,
    #[regex(r#"[a-zA-Z0-9_]+"#)]
    LITERAL(&'a str),
    #[regex(r#""[^"]*""#)]
    Text(&'a str),
    #[regex(r#"'[^']*'"#)]
    Text2(&'a str),
    #[regex(r#"(\d+)(\.\d+)?"#, priority = 3)]
    NUM(&'a str),
}

pub(crate) fn tuple<'a>(text: &str) -> IntMap<Str<'a>> {
    let result: IntMap<Str> = IntMap::default();
    let mut index: i64 = 1;
    let lexer = TupleToken::lexer(&text);
    for token in lexer.into_iter() {
        if let Ok(attribute) = token {
            match attribute {
                TupleToken::LBRACE | TupleToken::RBRACE | TupleToken::COMMA => {}
                TupleToken::LITERAL(literal) => {
                    result.insert(index, Str::from(literal.to_string()));
                    index = index + 1;
                }
                TupleToken::Text(text) => {
                    result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                    index = index + 1;
                }
                TupleToken::Text2(text) => {
                    result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                    index = index + 1;
                }
                TupleToken::NUM(num) => {
                    result.insert(index, Str::from(num.to_string()));
                    index = index + 1;
                }
            }
        }
    }
    result
}

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t\n\f]+")] // Ignore this regex pattern between tokens
enum ArrayToken<'a> {
    #[token("[")]
    LBRACKET,
    #[token("]")]
    RBRACKET,
    #[token(",")]
    COMMA,
    #[regex(r#"[a-zA-Z0-9_]+"#)]
    LITERAL(&'a str),
    #[regex(r#""[^"]*""#)]
    Text(&'a str),
    #[regex(r#"'[^']*'"#)]
    Text2(&'a str),
    #[regex(r#"(\d+)(\.\d+)?"#, priority = 3)]
    NUM(&'a str),
}

pub(crate) fn parse_array<'a>(text: &str) -> IntMap<Str<'a>> {
    let result: IntMap<Str> = IntMap::default();
    let mut index: i64 = 1;
    let lexer = ArrayToken::lexer(&text);
    for token in lexer.into_iter() {
        if let Ok(attribute) = token {
            match attribute {
                ArrayToken::LBRACKET | ArrayToken::RBRACKET | ArrayToken::COMMA => {}
                ArrayToken::LITERAL(literal) => {
                    result.insert(index, Str::from(literal.to_string()));
                    index = index + 1;
                }
                ArrayToken::Text(text) => {
                    result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                    index = index + 1;
                }
                ArrayToken::Text2(text) => {
                    result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                    index = index + 1;
                }
                ArrayToken::NUM(num) => {
                    result.insert(index, Str::from(num.to_string()));
                    index = index + 1;
                }
            }
        }
    }
    result
}

pub(crate) fn semver<'a>(text: &str) -> StrMap<'a, Str<'a>> {
    let version_obj: StrMap<Str> = StrMap::default();
    if let Ok(version) = Version::parse(text) {
        version_obj.insert(Str::from("major"), Str::from(version.major.to_string()));
        version_obj.insert(Str::from("minor"), Str::from(version.minor.to_string()));
        version_obj.insert(Str::from("patch"), Str::from(version.patch.to_string()));
        if !version.pre.is_empty() {
            version_obj.insert(Str::from("pre"), Str::from(version.pre.to_string()));
        }
        if !version.build.is_empty() {
            version_obj.insert(Str::from("build"), Str::from(version.build.to_string()));
        }
    }
    version_obj
}

pub(crate) fn variant<'a>(text: &str) -> StrMap<'a, Str<'a>> {
    let version_obj: StrMap<Str> = StrMap::default();
    let text = text.trim();
    if let Some(offset) = text.find('(') {
        let name = text[0..offset].trim();
        let inner = &text[offset + 1..];
        let mut value = inner.strip_suffix(')').unwrap_or(inner).trim().to_string();
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len() - 1].to_string();
        } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
            value = value[1..value.len() - 1].to_string();
        }
        version_obj.insert(Str::from("name".to_owned()), Str::from(name.to_string()));
        version_obj.insert(Str::from("value".to_owned()), Str::from(value));
    } else {
        version_obj.insert(Str::from("name".to_owned()), Str::from(text.to_string()));
    }
    version_obj
}

pub(crate) fn flags<'a>(text: &str) -> StrMap<'a, Int> {
    let flags_obj: StrMap<Int> = StrMap::default();
    if text.contains('{') {
        let text = text.trim();
        let parts = text[0..text.len() - 1].split(',');
        for part in parts {
            flags_obj.insert(Str::from(part.trim().to_string()), 1);
        }
    }
    flags_obj
}

const SUFFIX: [&str; 9] = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
const UNIT: f64 = 1024.0;

pub fn format_bytes(size: i64) -> String {
    let size = size as f64;
    if size < UNIT {
        return format!("{} B", size);
    }
    let base = size.log10() / UNIT.log10();
    let mut buffer = ryu::Buffer::new();
    let result = buffer
        .format((UNIT.powf(base - base.floor()) * 10.0).round() / 10.0)
        .trim_end_matches(".0");
    [result, SUFFIX[base.floor() as usize]].join(" ")
}

/// text: 111 B, 11.2 KB, 110KB, 1.5GB, 2 MiB, 3k
pub fn to_bytes(text: &str) -> i64 {
    let text = text.trim();
    let offset = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(text.len());
    let size = text[..offset].parse::<f64>().unwrap_or(0.0);
    let mut unit = text[offset..].trim().to_uppercase().replace('I', "");
    if unit.is_empty() {
        return size as i64;
    }
    if !unit.ends_with('B') {
        unit.push('B');
    }
    let index = SUFFIX.iter().position(|&r| r == unit).unwrap_or(0);
    (size * UNIT.powi(index as i32)) as i64
}

#[allow(unused_assignments)]
pub(crate) fn hex2rgb(text: &str) -> IntMap<Int> {
    let result: IntMap<Int> = IntMap::default();
    // convert hex str to decimal
    let hex_text = text.trim_start_matches("#");
    if !hex_text.bytes().all(|b| b.is_ascii_hexdigit()) {
        stdlib_warning("hex2rgb", format!("invalid hex color {:?}", text));
        return result;
    }
    let text_len = hex_text.len();
    let mut red = 0;
    let mut green = 0;
    let mut blue = 0;
    if text_len < 6 {
        let hex_text = format!("{:0<6}", hex_text);
        red = i64::from_str_radix(&hex_text[0..2], 16).unwrap_or(0);
        green = i64::from_str_radix(&hex_text[2..4], 16).unwrap_or(0);
        blue = i64::from_str_radix(&hex_text[4..6], 16).unwrap_or(0);
    } else {
        red = i64::from_str_radix(&hex_text[0..2], 16).unwrap_or(0);
        green = i64::from_str_radix(&hex_text[2..4], 16).unwrap_or(0);
        blue = i64::from_str_radix(&hex_text[4..6], 16).unwrap_or(0);
    }
    result.insert(1, red);
    result.insert(2, green);
    result.insert(3, blue);
    result
}

pub fn rgb2hex(red: i64, green: i64, blue: i64) -> String {
    let red = if red < 0 { 0 } else if red > 255 { 255 } else { red };
    let green = if green < 0 { 0 } else if green > 255 { 255 } else { green };
    let blue = if blue < 0 { 0 } else if blue > 255 { 255 } else { blue };
    format!("#{:02X}{:02X}{:02X}", red, green, blue)
}

/// eval context with https://lib.rs/crates/evalexpr
pub(crate)  fn eval_int_context<'a>(formula: &str, context: &StrMap<'a, Int>) -> Float {
    use evalexpr::*;
    let mut eval_context = HashMapContext::<DefaultNumericTypes>::new();
    context.iter(|map| {
        for (key, value) in map {
            let value: f64 = *value as f64;
            eval_context.set_value(key.to_string(), Value::Float(value)).unwrap();
        }
    });
    let Some(result) = run_eval(formula, &mut eval_context) else { return 0.0 };
    if let Ok(value) = result.as_float() {
        value
    } else if let Ok(value) = result.as_int() {
        value as f64
    } else {
        0.0
    }
}

pub(crate) fn eval_float_context<'a>(formula: &str, context: &StrMap<'a, Float>) -> Float {
    use evalexpr::*;
    let mut eval_context = HashMapContext::<DefaultNumericTypes>::new();
    context.iter(|map| {
        for (key, value) in map {
            eval_context.set_value(key.to_string(), Value::Float(*value)).unwrap();
        }
    });
    run_eval(formula, &mut eval_context).map_or(0.0, |result| value_to_float(&result))
}
pub(crate)  fn eval_context<'a>(formula: &str, context: &StrMap<'a, Str<'a>>) -> Float {
    use evalexpr::*;
    let mut eval_context = HashMapContext::<DefaultNumericTypes>::new();
    context.iter(|map| {
        for (key, value) in map {
            let value: f64 = value.as_str().parse().unwrap_or(0.0);
            eval_context.set_value(key.to_string(), Value::Float(value)).unwrap();
        }
    });
    run_eval(formula, &mut eval_context).map_or(0.0, |result| value_to_float(&result))
}


pub fn eval(formula: &str) -> Float {
    use evalexpr::*;
    let mut eval_context = HashMapContext::<DefaultNumericTypes>::new();
    run_eval(formula, &mut eval_context).map_or(0.0, |result| value_to_float(&result))
}

/// Evaluates `formula`, warning (once) and returning `None` if it is malformed.
fn run_eval(
    formula: &str,
    context: &mut evalexpr::HashMapContext<evalexpr::DefaultNumericTypes>,
) -> Option<evalexpr::Value<evalexpr::DefaultNumericTypes>> {
    match evalexpr::eval_with_context_mut(formula, context) {
        Ok(value) => Some(value),
        Err(e) => {
            crate::runtime::stdlib_warning("eval", format!("{:?}: {}", formula, e));
            None
        }
    }
}

fn value_to_float(value: &Value<DefaultNumericTypes>) -> Float {
    if let Ok(value) = value.as_float() {
        value
    } else if let Ok(value) = value.as_int() {
        value as f64
    } else if let Ok(value) = value.as_boolean() {
        if value {
            1f64
        } else {
            0f64
        }
    } else {
        0f64
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_min_max() {
        // numbers
        assert_eq!(min("2", "1", ""), "1");
        assert_eq!(max("2", "10", ""), "10");
        assert_eq!(min("2", "1", "1"), "1");
        assert_eq!(min("1", "1", "2"), "1");
        assert_eq!(max("1", "2", "2"), "2");
        assert_eq!(max("3", "3", "1"), "3");
        assert_eq!(min("-1.5", "0", "2"), "-1.5");
        // ties keep the earliest item
        assert_eq!(min("1.0", "1", ""), "1.0");
        assert_eq!(max("1", "1.0", "0"), "1");
        // strings
        assert_eq!(max("a", "b", ""), "b");
        assert_eq!(min("b", "a", ""), "a");
        assert_eq!(min("b", "c", "a"), "a");
        assert_eq!(max("b", "c", "a"), "c");
        assert_eq!(min("b", "b", "a"), "a");
        // mixed numbers and strings compare as strings
        assert_eq!(min("10", "9", "x"), "10");
        assert_eq!(max("10", "9", "x"), "x");
    }

    #[test]
    fn test_mkbool() {
        assert_eq!(mkbool("true"), 1);
        assert_eq!(mkbool("True"), 1);
        assert_eq!(mkbool(" 0 "), 0);
        assert_eq!(mkbool("0.0"), 0);
        assert_eq!(mkbool("yes"), 1);
        assert_eq!(mkbool(""), 0);
        assert_eq!(mkbool("✓"), 1);
    }

    #[test]
    fn test_uuid() {
        println!("{}", uuid("v7"));
    }

    #[test]
    fn test_tsid() {
        println!("{}", tsid());
    }

    #[test]
    fn test_seq() {
        let result = seq(1.0, 1.0, 10.0).unwrap();
        println!("{:?}", result);
    }

    #[test]
    fn test_awk_strtonum() {
        let n = |s: &str| awk_strtonum(s.as_bytes(), false);
        assert_eq!(n("0x1F"), 31.0);
        assert_eq!(n("0X1f"), 31.0);
        assert_eq!(n("0x1Fzz"), 31.0);
        assert_eq!(n("0x"), 0.0);
        assert_eq!(n("017"), 15.0);
        assert_eq!(n("0017"), 15.0);
        assert_eq!(n("017abc"), 15.0);
        assert_eq!(n("018"), 18.0);
        assert_eq!(n("0179"), 179.0);
        assert_eq!(n("017.5"), 17.5);
        assert_eq!(n("017e1"), 170.0);
        assert_eq!(n("00"), 0.0);
        assert_eq!(n("08"), 8.0);
        assert_eq!(n(" 017"), 17.0);
        assert_eq!(n("+017"), 17.0);
        assert_eq!(n("-017"), -17.0);
        assert_eq!(n("12abc"), 12.0);
        assert_eq!(n("abc"), 0.0);
        assert_eq!(n(""), 0.0);
        // A strnum that looks like a number is that number; other strnums follow the rules above.
        assert_eq!(awk_strtonum(b"017", true), 17.0);
        // Hexadecimal input is a number too (as everywhere in zawk).
        assert_eq!(awk_strtonum(b" 0x11 ", true), 17.0);
        assert_eq!(awk_strtonum(b"0x11", true), 17.0);
        assert_eq!(awk_strtonum(b"017abc", true), 15.0);
    }

    #[test]
    fn test_shlex() {
        let text = "echo hello world";
        let args = shlex(text);
        println!("{:?}", args);
    }

    #[test]
    fn test_isint() {
        assert!(is_str_int("11"));
        assert!(is_str_int("0x11"));
        assert!(is_str_int("-0X1f"));
        assert!(!is_str_int("0x-5"));
        assert!(!is_str_int("0b11"));
        assert!(!is_str_int("11.1"));
    }

    #[test]
    fn test_isnum() {
        assert!(is_str_num("11.01"));
        assert!(is_str_num("0x11"));
        assert!(!is_str_num("0x"));
        assert!(!is_str_num("0o17"));
        assert!(!is_str_num("u11.1"));
    }

    #[test]
    fn test_snowflake() {
        let machine_id: i64 = 1000;
        println!("{}", snowflake(machine_id as u16));
    }

    #[test]
    fn test_semver() {
        let map = semver("1.2.3-beta1");
        println!("{:?}", map);
    }

    #[test]
    fn test_machine_node() {
        let machine_id = 64;
        let new_machine_id = machine_id >> 5;
        let new_node_id = machine_id & (32 - 1);
        println!("{} {}", new_machine_id, new_node_id);
    }

    #[test]
    fn test_format_bytes() {
        let size = 110;
        println!("{}", format_bytes(size));
    }

    #[test]
    fn test_to_bytes() {
        assert_eq!(to_bytes("123"), 123);
        assert_eq!(to_bytes("111 B"), 111);
        assert_eq!(to_bytes("123 kb"), 123 * 1024);
        assert_eq!(to_bytes("1.5GB"), 1610612736);
        assert_eq!(to_bytes("1.5 GB"), 1610612736);
        assert_eq!(to_bytes(" 2 MiB "), 2 * 1024 * 1024);
        assert_eq!(to_bytes("3k"), 3 * 1024);
        assert_eq!(to_bytes("0.5KB"), 512);
        assert_eq!(to_bytes("abc"), 0);
    }

    #[test]
    fn test_parse_array() {
        let text = "[0 1 'two' 3]";
        let array = parse_array(text);
        assert_eq!(array.len(), 4);
        assert_eq!(array.get(&3).as_str(), "two");
    }

    #[test]
    fn test_hex2rgb() {
        let text = "#FFaa5";
        let rgb = hex2rgb(text);
        println!("{:?}", rgb);
    }
    #[test]
    fn test_rgb2hex() {
        let red = 255;
        let green = 170;
        let blue = 85;
        let hex = rgb2hex(red, green, blue);
        println!("{}", hex);
    }

    #[test]
    fn test_eval_context() {
        let context: StrMap<Str> = StrMap::default();
        context.insert(Str::from("x"), Str::from("7"));
        let result = eval_context("1 + 1", &context);
        println!("{}", result);
    }
}
