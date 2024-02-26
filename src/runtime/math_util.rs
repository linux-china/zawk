use std::collections::HashMap;
use std::sync::Mutex;
use lazy_static::lazy_static;
use semver::{Version};
use sonyflake::Sonyflake;
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
    static ref SNOWFLAKES: Mutex<HashMap<u16, Sonyflake>> = Mutex::new(HashMap::new());
}

pub(crate) fn snowflake(machine_id: u16) -> Int {
    let mut pool = SNOWFLAKES.lock().unwrap();
    let generator = pool.entry(machine_id).or_insert_with(|| {
        Sonyflake::builder().machine_id(&|| { Ok(machine_id) }).finalize().unwrap()
    });
    generator.next_id().unwrap() as Int
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
        let machine_id: i64 = 234342347234;
        println!("{}", machine_id as u16);
        println!("{}", snowflake(machine_id as u16));
    }

    #[test]
    fn test_semver() {
        let map = semver("1.2.3-beta1");
        println!("{:?}", map);
    }
}