//! `reverse`, `ltrim`, `rtrim`, `replace`, `translate`, `left`, `right`, `chr`, `ord`,
//! `path_join` and `bar`: string functions that mirror DuckDB's functions of the same name.
//!
//! They work on characters: bytes that are not valid UTF-8 are kept, each one counting as one
//! character, as in `length` and `substr`.

use unicode_segmentation::UnicodeSegmentation;

use crate::runtime::float_parse::str_to_num;
use crate::runtime::{Float, Int, Str};

/// The string functions that take up to three strings and return a string. Arguments a function
/// does not take are passed as empty strings, and numbers (`left(s, n)`, `chr(n)`) as their text.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum TextOp {
    Reverse,
    LTrim,
    RTrim,
    Replace,
    Translate,
    Left,
    Right,
    Chr,
    PathJoin,
}

impl TextOp {
    const ALL: [TextOp; 9] = [
        TextOp::Reverse,
        TextOp::LTrim,
        TextOp::RTrim,
        TextOp::Replace,
        TextOp::Translate,
        TextOp::Left,
        TextOp::Right,
        TextOp::Chr,
        TextOp::PathJoin,
    ];

    pub fn func_name(self) -> &'static str {
        match self {
            TextOp::Reverse => "reverse",
            TextOp::LTrim => "ltrim",
            TextOp::RTrim => "rtrim",
            TextOp::Replace => "replace",
            TextOp::Translate => "translate",
            TextOp::Left => "left",
            TextOp::Right => "right",
            TextOp::Chr => "chr",
            TextOp::PathJoin => "path_join",
        }
    }

    /// The number of arguments the function takes when called by name, as `(required, max)`.
    pub fn arg_range(self) -> (usize, usize) {
        match self {
            TextOp::Reverse | TextOp::Chr => (1, 1),
            TextOp::LTrim | TextOp::RTrim => (1, 2),
            TextOp::Left | TextOp::Right | TextOp::PathJoin => (2, 2),
            TextOp::Replace | TextOp::Translate => (3, 3),
        }
    }

    /// The code passed to the compiled code's intrinsic.
    pub fn code(self) -> Int {
        TextOp::ALL.iter().position(|op| *op == self).unwrap() as Int
    }

    pub fn from_code(code: Int) -> Option<TextOp> {
        usize::try_from(code)
            .ok()
            .and_then(|i| TextOp::ALL.get(i).copied())
    }
}

/// `op(a, b, c)` on strings: the entry point of both backends.
pub(crate) fn eval_str<'b>(op: TextOp, a: &Str, b: &Str<'b>, c: &Str) -> Str<'b> {
    match op {
        TextOp::LTrim => a.trim_sides(b, true, false),
        TextOp::RTrim => a.trim_sides(b, false, true),
        _ => a.with_bytes(|a| {
            b.with_bytes(|b| c.with_bytes(|c| Str::from_bytes(&eval(op, a, b, c))))
        }),
    }
}

/// `op(a, b, c)`; `LTrim` and `RTrim` are done by `Str::trim_sides`, not here.
pub fn eval(op: TextOp, a: &[u8], b: &[u8], c: &[u8]) -> Vec<u8> {
    match op {
        TextOp::Reverse => reverse(a),
        TextOp::Replace => replace(a, b, c),
        TextOp::Translate => translate(a, b, c),
        TextOp::Left => left(a, to_int(b)),
        TextOp::Right => right(a, to_int(b)),
        TextOp::Chr => chr(to_int(a)),
        TextOp::PathJoin => path_join(a, b),
        TextOp::LTrim | TextOp::RTrim => unreachable!("ltrim/rtrim are Str::trim_sides"),
    }
}

/// A number given as text, truncated toward zero: `"2.9"` is 2.
fn to_int(bs: &[u8]) -> Int {
    let n = str_to_num(bs);
    if n.is_nan() { 0 } else { n as Int }
}

/// The characters of `bs`: a valid UTF-8 character, or a single byte that is not.
fn units(bs: &[u8]) -> Vec<&[u8]> {
    let mut res = Vec::with_capacity(bs.len());
    for chunk in bs.utf8_chunks() {
        let valid = chunk.valid();
        res.extend(
            valid
                .char_indices()
                .map(|(i, c)| &valid.as_bytes()[i..i + c.len_utf8()]),
        );
        res.extend(chunk.invalid().chunks(1));
    }
    res
}

/// `reverse(s)`: the characters of `s` in reverse order. A character with combining marks or an
/// emoji sequence (a grapheme cluster) is kept as one, so `reverse("e\u{301}a")` is `"ae\u{301}"`.
pub fn reverse(bs: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for chunk in bs.utf8_chunks() {
        parts.extend(chunk.valid().graphemes(true).map(str::as_bytes));
        parts.extend(chunk.invalid().chunks(1));
    }
    parts.into_iter().rev().flatten().copied().collect()
}

/// `replace(s, from, to)`: every occurrence of the text `from` replaced by `to`, from left to
/// right without overlaps. An empty `from` leaves `s` unchanged.
pub fn replace(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return s.to_vec();
    }
    let mut res = Vec::with_capacity(s.len());
    let mut last = 0;
    for i in memchr::memmem::find_iter(s, from) {
        res.extend_from_slice(&s[last..i]);
        res.extend_from_slice(to);
        last = i + from.len();
    }
    res.extend_from_slice(&s[last..]);
    res
}

/// `translate(s, from, to)`: each character of `s` that is in `from` replaced by the character
/// at the same position in `to`, or removed when `to` is shorter. When a character is repeated
/// in `from`, its first position counts.
pub fn translate(s: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let from = units(from);
    let to = units(to);
    let mut res = Vec::with_capacity(s.len());
    for u in units(s) {
        match from.iter().position(|f| *f == u) {
            Some(i) => {
                if let Some(t) = to.get(i) {
                    res.extend_from_slice(t);
                }
            }
            None => res.extend_from_slice(u),
        }
    }
    res
}

/// The byte offset of the start of character `n` of `bs` (the length of `bs` past its end).
fn char_offset(us: &[&[u8]], n: usize) -> usize {
    us.iter().take(n).map(|u| u.len()).sum()
}

/// `left(s, n)`: the first `n` characters of `s`; all but the last `-n` when `n` is negative.
pub fn left(s: &[u8], n: Int) -> Vec<u8> {
    let us = units(s);
    let len = us.len() as Int;
    let n = if n < 0 { (len + n).max(0) } else { n.min(len) };
    s[..char_offset(&us, n as usize)].to_vec()
}

/// `right(s, n)`: the last `n` characters of `s`; all but the first `-n` when `n` is negative.
pub fn right(s: &[u8], n: Int) -> Vec<u8> {
    let us = units(s);
    let len = us.len() as Int;
    let skip = if n < 0 {
        (-n).min(len)
    } else {
        (len - n).max(0)
    };
    s[char_offset(&us, skip as usize)..].to_vec()
}

/// `chr(n)`: the character with code point `n`; `""` when `n` is not one (negative, a surrogate,
/// or above `0x10FFFF`).
pub fn chr(n: Int) -> Vec<u8> {
    u32::try_from(n)
        .ok()
        .and_then(char::from_u32)
        .map_or_else(Vec::new, |c| c.to_string().into_bytes())
}

/// `ord(s)`: the code point of the first character of `s`, the value of the byte when it is not
/// valid UTF-8, and 0 for an empty string.
pub fn ord(s: &[u8]) -> Int {
    match s.utf8_chunks().next() {
        None => 0,
        Some(chunk) => match chunk.valid().chars().next() {
            Some(c) => c as Int,
            None => chunk.invalid().first().map_or(0, |b| *b as Int),
        },
    }
}

/// `path_join(a, b)`: `a` and `b` joined with a single `/`. An empty part is skipped, and an
/// absolute `b` (starting with `/`) replaces `a`, as Python's `os.path.join`.
pub fn path_join(a: &[u8], b: &[u8]) -> Vec<u8> {
    if b.is_empty() {
        return a.to_vec();
    }
    if a.is_empty() || b[0] == b'/' {
        return b.to_vec();
    }
    let mut res = a.to_vec();
    if res.last() != Some(&b'/') {
        res.push(b'/');
    }
    res.extend_from_slice(b);
    res
}

/// The widest bar drawn by `bar`, so that a typo in the width does not fill the memory.
const MAX_BAR_WIDTH: Float = 1000.0;

/// `bar(x, min, max, width)`: a horizontal bar of `width` characters, full for `max` and empty
/// for `min` (`x` is clamped to the range), drawn with Unicode blocks in steps of 1/8 of a
/// character and padded with spaces to `width`, as DuckDB's `bar`. Empty when `width` is not
/// positive, `max` is not above `min`, or a number is NaN.
pub fn bar(x: Float, min: Float, max: Float, width: Float) -> String {
    const PARTIAL: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let width = width.min(MAX_BAR_WIDTH).trunc();
    if width.is_nan() || width < 1.0 || x.is_nan() || !(max > min) {
        return String::new();
    }
    let width = width as usize;
    let ratio = ((x - min) / (max - min)).clamp(0.0, 1.0);
    // in eighths of a character
    let eighths = (ratio * (width * 8) as Float).round() as usize;
    let (full, part) = (eighths / 8, eighths % 8);
    let mut res = String::with_capacity(width * 3);
    res.extend(std::iter::repeat_n('█', full));
    if part > 0 {
        res.push(PARTIAL[part]);
    }
    let drawn = full + (part > 0) as usize;
    res.extend(std::iter::repeat_n(' ', width - drawn));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(op: TextOp, a: &str, b: &str, c: &str) -> String {
        String::from_utf8(eval(op, a.as_bytes(), b.as_bytes(), c.as_bytes())).unwrap()
    }

    #[test]
    fn codes_round_trip() {
        for op in TextOp::ALL {
            assert_eq!(TextOp::from_code(op.code()), Some(op));
        }
        assert_eq!(TextOp::from_code(-1), None);
        assert_eq!(TextOp::from_code(99), None);
    }

    #[test]
    fn reverse_keeps_characters() {
        assert_eq!(text(TextOp::Reverse, "", "", ""), "");
        assert_eq!(text(TextOp::Reverse, "hello", "", ""), "olleh");
        assert_eq!(text(TextOp::Reverse, "你好世界", "", ""), "界世好你");
        assert_eq!(text(TextOp::Reverse, "e\u{301}a", "", ""), "ae\u{301}");
        assert_eq!(reverse(b"ab\xe9c"), b"c\xe9ba");
    }

    #[test]
    fn replace_is_literal() {
        assert_eq!(text(TextOp::Replace, "a.b.c", ".", "::"), "a::b::c");
        assert_eq!(text(TextOp::Replace, "aaa", "aa", "b"), "ba");
        assert_eq!(text(TextOp::Replace, "abc", "", "x"), "abc");
        assert_eq!(text(TextOp::Replace, "$1 $2", "$", ""), "1 2");
    }

    #[test]
    fn translate_maps_characters() {
        assert_eq!(text(TextOp::Translate, "12345", "143", "ax"), "a2x5");
        assert_eq!(text(TextOp::Translate, "hello", "el", "ip"), "hippo");
        assert_eq!(text(TextOp::Translate, "aab", "aa", "xy"), "xxb");
        assert_eq!(text(TextOp::Translate, "你好", "好", "们"), "你们");
    }

    #[test]
    fn left_and_right() {
        assert_eq!(text(TextOp::Left, "hello", "2", ""), "he");
        assert_eq!(text(TextOp::Left, "hello", "-2", ""), "hel");
        assert_eq!(text(TextOp::Left, "hello", "10", ""), "hello");
        assert_eq!(text(TextOp::Left, "hello", "-10", ""), "");
        assert_eq!(text(TextOp::Left, "你好世界", "1", ""), "你");
        assert_eq!(text(TextOp::Right, "hello", "2", ""), "lo");
        assert_eq!(text(TextOp::Right, "hello", "-2", ""), "llo");
        assert_eq!(text(TextOp::Right, "hello", "10", ""), "hello");
        assert_eq!(text(TextOp::Right, "hello", "0", ""), "");
        assert_eq!(text(TextOp::Right, "你好世界", "2", ""), "世界");
    }

    #[test]
    fn chr_and_ord() {
        assert_eq!(text(TextOp::Chr, "65", "", ""), "A");
        assert_eq!(text(TextOp::Chr, "20320", "", ""), "你");
        assert_eq!(text(TextOp::Chr, "-1", "", ""), "");
        assert_eq!(text(TextOp::Chr, "55296", "", ""), "");
        assert_eq!(ord(b"A"), 65);
        assert_eq!(ord("你好".as_bytes()), 20320);
        assert_eq!(ord(b""), 0);
        assert_eq!(ord(b"\xe9x"), 0xe9);
    }

    #[test]
    fn path_join_parts() {
        assert_eq!(text(TextOp::PathJoin, "a", "b", ""), "a/b");
        assert_eq!(text(TextOp::PathJoin, "a/", "b", ""), "a/b");
        assert_eq!(text(TextOp::PathJoin, "a", "/b", ""), "/b");
        assert_eq!(text(TextOp::PathJoin, "", "b", ""), "b");
        assert_eq!(text(TextOp::PathJoin, "a", "", ""), "a");
        assert_eq!(text(TextOp::PathJoin, "/", "b", ""), "/b");
    }

    #[test]
    fn bars() {
        assert_eq!(bar(5.0, 0.0, 10.0, 10.0), "█████     ");
        assert_eq!(bar(10.0, 0.0, 10.0, 4.0), "████");
        assert_eq!(bar(20.0, 0.0, 10.0, 4.0), "████");
        assert_eq!(bar(-1.0, 0.0, 10.0, 4.0), "    ");
        assert_eq!(bar(7.0, 0.0, 8.0, 1.0), "▉");
        assert_eq!(bar(1.0, 0.0, 8.0, 1.0), "▏");
        assert_eq!(bar(0.5, 0.0, 8.0, 8.0), "▌       ");
        assert_eq!(bar(1.0, 1.0, 1.0, 4.0), "");
        assert_eq!(bar(1.0, 0.0, 2.0, 0.0), "");
        assert_eq!(bar(Float::NAN, 0.0, 2.0, 4.0), "");
    }
}
