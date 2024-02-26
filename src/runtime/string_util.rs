use pad::{Alignment, PadStr};
use unicode_segmentation::UnicodeSegmentation;
use crate::runtime::{IntMap, Str};

pub fn pad_left(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Left, false)
}

pub fn pad_right(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Right, false)
}

pub fn pad_both(text: &str, len: usize, pad: &str) -> String {
    if text.len() > len {
        return text[0..len].to_string();
    }
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::MiddleRight, false)
}

pub fn strcmp(text1: &str, text2: &str) -> i64 {
    return if text1 == text2 {
        0
    } else if text1 < text2 {
        -1
    } else {
        1
    };
}

pub(crate) fn words<'a>(text: &str) -> IntMap<Str<'a>> {
    let result: IntMap<Str> = IntMap::default();
    let mut index: i64 = 1;
    for word in text.unicode_words() {
        result.insert(index, Str::from(word.to_string()));
        index = index + 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use unicode_segmentation::UnicodeSegmentation;
    use super::*;

    #[test]
    fn test_pad_left() {
        let text = pad_left("hello", 100, "*");
        println!("{}", text);
    }

    #[test]
    fn test_strcmp() {
        let text1 = "hello";
        let text2 = "Hello";
        println!("{}", strcmp(text1, text2));
    }

    #[test]
    fn test_words() {
        let text = "Hello , world! could you give a 名称?";
        let words = text.unicode_words();
        for word in words {
            println!("{}", word);
        }
    }
}