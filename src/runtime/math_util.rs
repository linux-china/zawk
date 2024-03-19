use std::collections::HashMap;
use std::sync::Mutex;
use lazy_static::lazy_static;
use logos::Logos;
use semver::{Version};
use snowflake::SnowflakeIdGenerator;
use crate::runtime::{Float, Int, IntMap, Str, StrMap};

pub fn min(first: &str, second: &str, third: &str) -> String {
    let num1_result = first.parse::<f64>();
    let num2_result = second.parse::<f64>();
    if third.is_empty() { // only 2 params
        return if num1_result.is_ok() && num2_result.is_ok() {
            if num1_result.unwrap() < num2_result.unwrap() {
                first
            } else {
                second
            }
        } else {
            if first < third {
                first
            } else {
                second
            }
        }.to_string();
    } else { // 3 params
        let num3_result = third.parse::<f64>();
        return if num1_result.is_ok() && num2_result.is_ok() && num3_result.is_ok() {
            let num1 = num1_result.unwrap();
            let num2 = num2_result.unwrap();
            let num3 = num3_result.unwrap();
            if num1 < num2 && num1 < num3 {
                first
            } else if num2 < num1 && num2 < num3 {
                second
            } else if num3 < num1 && num3 < num2 {
                third
            } else {
                first
            }
        } else {
            if first < second && first < second {
                first
            } else if second < second && second < third {
                second
            } else if third < first && third < second {
                third
            } else {
                first
            }
        }.to_string();
    }
}

pub fn max(first: &str, second: &str, third: &str) -> String {
    let num1_result = first.parse::<f64>();
    let num2_result = second.parse::<f64>();
    if third.is_empty() { // only 2 params
        return if num1_result.is_ok() && num2_result.is_ok() {
            if num1_result.unwrap() > num2_result.unwrap() {
                first
            } else {
                second
            }
        } else {
            if first > third {
                first
            } else {
                second
            }
        }.to_string();
    } else { // 3 params
        let num3_result = third.parse::<f64>();
        return if num1_result.is_ok() && num2_result.is_ok() && num3_result.is_ok() {
            let num1 = num1_result.unwrap();
            let num2 = num2_result.unwrap();
            let num3 = num3_result.unwrap();
            if num1 > num2 && num1 > num3 {
                first
            } else if num2 > num1 && num2 > num3 {
                second
            } else if num3 > num1 && num3 > num2 {
                third
            } else {
                first
            }
        } else {
            if first > second && first > second {
                first
            } else if second > second && second > third {
                second
            } else if third > first && third > second {
                third
            } else {
                first
            }
        }.to_string();
    }
}

pub(crate) fn map_int_int_asort(obj: &IntMap<Int>, target_obj: &IntMap<Int>) {
    let mut items: Vec<Int> = vec![];
    for index in obj.to_vec() {
        items.push(obj.get(&index));
    }
    items.sort();
    if target_obj.len() > 0 {
        target_obj.clear();
        let mut index = 1;
        for item in items {
            target_obj.insert(index, item);
            index += 1;
        }
    } else {
        obj.clear();
        let mut index = 1;
        for item in items {
            obj.insert(index, item);
            index += 1;
        }
    }
}

pub(crate) fn map_int_float_asort(obj: &IntMap<Float>, target_obj: &IntMap<Float>) {
    let mut items: Vec<Float> = vec![];
    for index in obj.to_vec() {
        items.push(obj.get(&index));
    }
    if target_obj.len() > 0 {
        target_obj.clear();
        let mut index = 1;
        for item in items {
            target_obj.insert(index, item);
            index += 1;
        }
    } else {
        obj.clear();
        let mut index = 1;
        for item in items {
            obj.insert(index, item);
            index += 1;
        }
    }
}

pub(crate) fn map_int_str_asort(obj: &IntMap<Str>, target_obj: &IntMap<Str>) {
    let mut items: Vec<String> = vec![];
    for index in obj.to_vec() {
        items.push(obj.get(&index).to_string());
    }
    if target_obj.len() > 0 {
        target_obj.clear();
        let mut index = 1;
        for item in items {
            target_obj.insert(index, Str::from(item));
            index += 1;
        }
    } else {
        obj.clear();
        let mut index = 1;
        for item in items {
            obj.insert(index, Str::from(item));
            index += 1;
        }
    }
}

pub(crate) fn map_int_int_join(obj: &IntMap<Int>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.reverse();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.join(sep)
}

pub(crate) fn map_int_float_join(obj: &IntMap<Float>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.reverse();
    for index in keys {
        items.push(obj.get(&index).to_string());
    }
    items.join(sep)
}

pub(crate) fn map_int_str_join(obj: &IntMap<Str>, sep: &str) -> String {
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.reverse();
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

pub(crate) fn seq(start: Float, step: Float, end: Float) -> IntMap<Float> {
    let result: IntMap<Float> = IntMap::default();
    let mut ir = start;
    let mut index = 1;
    while ir <= end {
        result.insert(index, ir);
        ir += step;
        index += 1;
    }
    result
}

pub(crate) fn uuid(version: &str) -> String {
    match version {
        "v7" => uuid::Uuid::now_v7().to_string(),
        "v4" | &_ => uuid::Uuid::new_v4().to_string()
    }
}

lazy_static! {
    static ref SNOWFLAKES: Mutex<HashMap<u16, SnowflakeIdGenerator>> = Mutex::new(HashMap::new());
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
    ulid::Ulid::new().to_string()
}

pub(crate) fn strtonum(text: &str) -> Float {
    let text = text.trim().to_lowercase();
    return if text.starts_with("0x") {
        i64::from_str_radix(&text[2..], 16).unwrap_or(0) as f64
    } else if text.starts_with("0o") {
        i64::from_str_radix(&text[2..], 8).unwrap_or(0) as f64
    } else if text.starts_with("0b") {
        i64::from_str_radix(&text[2..], 2).unwrap_or(0) as f64
    } else {
        text.parse::<f64>().unwrap_or(0.0)
    };
}

pub(crate) fn strtoint(text: &str) -> Int {
    let text = text.trim().to_lowercase();
    return if text.starts_with("0x") {
        i64::from_str_radix(&text[2..], 16).unwrap_or(0)
    } else if text.starts_with("0o") {
        i64::from_str_radix(&text[2..], 8).unwrap_or(0)
    } else if text.starts_with("0b") {
        i64::from_str_radix(&text[2..], 2).unwrap_or(0)
    } else {
        text.parse::<i64>().unwrap_or(0)
    };
}

pub(crate) fn is_str_int(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    if text.starts_with("0x") {
        i64::from_str_radix(&text[2..], 16).is_ok()
    } else if text.starts_with("0o") {
        i64::from_str_radix(&text[2..], 8).is_ok()
    } else if text.starts_with("0b") {
        i64::from_str_radix(&text[2..], 2).is_ok()
    } else {
        text.parse::<i64>().is_ok()
    }
}

pub(crate) fn is_str_num(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    if text.starts_with("0x") {
        i64::from_str_radix(&text[2..], 16).is_ok()
    } else if text.starts_with("0o") {
        i64::from_str_radix(&text[2..], 8).is_ok()
    } else if text.starts_with("0b") {
        i64::from_str_radix(&text[2..], 2).is_ok()
    } else {
        text.parse::<f64>().is_ok()
    }
}

pub(crate) fn uniq<'a>(obj: &IntMap<Str<'a>>, _param: &str) -> IntMap<Str<'a>> {
    //todo uniq implement logic with param
    let mut items: Vec<String> = vec![];
    let mut keys = obj.to_vec().clone();
    keys.reverse();
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
    #[regex(r#"[a-zA-Z0-9_]*"#)]
    LITERAL(&'a str),
    #[regex(r#""[^"]*""#)]
    Text(&'a str),
    #[regex(r#"'[^']*'"#)]
    Text2(&'a str),
    #[regex(r#"(\d+)(\.\d+)?"#)]
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
    if let Some(offset) = text.trim().find('(') {
        let name = &text[0..offset].trim();
        let value = &text[offset + 1..text.len() - 1].trim();
        version_obj.insert(Str::from("name".to_owned()), Str::from(name.to_string()));
        version_obj.insert(Str::from("value".to_owned()), Str::from(value.to_string()));
    } else {
        version_obj.insert(Str::from("name".to_owned()), Str::from(text.to_string()));
    }
    version_obj
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

/// text: 111 B, 11.2 KB 110KB
pub fn to_bytes(text: &str) -> i64 {
    let text_len = text.len();
    if text_len < 2 {
        return text.parse::<i64>().unwrap_or(0);
    }
    let unit = &text[text_len - 2..].to_uppercase();
    if !unit.ends_with("B") {
        return text.parse::<i64>().unwrap_or(0);
    }
    // get index from SUFFIX
    let index = SUFFIX.iter().position(|&r| r == unit).unwrap_or(0);
    let unit = SUFFIX[index];
    let num_text = text[0..(text_len - unit.len())].trim();
    let size = num_text.parse::<f64>().unwrap_or(0.0);
    if index == 0 {
        size as i64
    } else {
        (size * (UNIT.powi(index as i32))) as i64
    }
}


#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_seq() {
        let result = seq(1.0, 1.0, 10.0);
        println!("{:?}", result);
    }

    #[test]
    fn test_strtonum() {
        assert_eq!(17f64, strtonum("0x11"));
        assert_eq!(3f64, strtonum("0b11"));
        assert_eq!(17f64, strtonum("17"));
        assert_eq!(17.2f64, strtonum("17.2"));
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
        assert!(!is_str_int("11.1"));
    }

    #[test]
    fn test_isnum() {
        assert!(is_str_num("11.01"));
        assert!(is_str_num("0x11"));
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
        let text = "123 B";
        println!("{}", to_bytes(text));
    }
}
