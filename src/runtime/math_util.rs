use crate::runtime::{Float, Int, IntMap, Str};

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

const YES: &'static [&'static str] = &["true", "yes", "1", "1.0", "✓"];
const NO: &'static [&'static str] = &["false", "no", "𐄂", "0", "0.0", "0.00", "00.0",
    "0x0", "0x00","0X0", "0X00", "0o0", "0o00", "0O0", "0O00", "0b0", "0b00","0B0", "0B00"];

pub(crate) fn mkbool(text: &str) -> i64 {
    let text = text.trim().to_lowercase();
    return if text.is_empty() || NO.contains(&text.as_str()) {
        0
    } else {
        1
    };
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
}