use pad::{Alignment, PadStr};

pub fn pad_left(text: &str, len: usize, pad: &str) -> String {
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Left, false)
}

pub fn pad_right(text: &str, len: usize, pad: &str) -> String {
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::Right, false)
}

pub fn pad_both(text: &str, len: usize, pad: &str) -> String {
    let pad_char = pad.chars().next().unwrap();
    text.pad(len, pad_char, Alignment::MiddleRight, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pad_left() {
        let text = pad_both("hello", 100, "*");
        println!("{}", text);
    }
}