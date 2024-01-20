use crate::runtime::{Float, Str};

pub fn min<'a>(first: &'a Str<'a>, second: &'a Str<'a>, third: &'a Str<'a>) -> &'a Str<'a> {
    let num1_text = first.as_str();
    let num2_text = second.as_str();
    let num1_result = num1_text.parse::<Float>();
    let num2_result = num2_text.parse::<Float>();
    if third.is_empty() { // only 2 params
        return if num1_result.is_ok() && num2_result.is_ok() {
            if num1_result.unwrap() < num2_result.unwrap() {
                first
            } else {
                second
            }
        } else {
            if num1_text < num2_text {
                first
            } else {
                second
            }
        };
    } else { // 3 params
        let num3_text = third.as_str();
        let num3_result = num3_text.parse::<Float>();
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
            if num1_text < num2_text && num1_text < num3_text {
                first
            } else if num2_text < num1_text && num2_text < num3_text {
                second
            } else if num3_text < num1_text && num3_text < num2_text {
                third
            } else {
                first
            }
        }
    }
}

pub fn max<'a>(first: &'a Str<'a>, second: &'a Str<'a>, third: &'a Str<'a>) -> &'a Str<'a> {
    let num1_text = first.as_str();
    let num2_text = second.as_str();
    let num1_result = num1_text.parse::<Float>();
    let num2_result = num2_text.parse::<Float>();
    if third.is_empty() { // only 2 params
        return if num1_result.is_ok() && num2_result.is_ok() {
            if (num1_result.unwrap() > num2_result.unwrap()) {
                first
            } else {
                second
            }
        } else {
            if num1_text > num2_text {
                first
            } else {
                second
            }
        };
    } else { // 3 params
        let num3_text = third.as_str();
        let num3_result = num3_text.parse::<Float>();
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
            if num1_text > num2_text && num1_text > num3_text {
                first
            } else if num2_text > num1_text && num2_text > num3_text {
                second
            } else if num3_text > num1_text && num3_text > num2_text {
                third
            } else {
                first
            }
        };
    }
}