use crate::common::Result;
use crate::runtime::{check_result_len, stdlib_warning, Float, Int, IntMap, SharedMap, Str, StrMap};
use pad::Alignment;
use std::sync::LazyLock;

/// awk `substr` with 1-based `pos`, counting chars. As in gawk and onetrue awk, a `pos` before
/// the first char starts at the first char and keeps `len`: `substr("hello", -1, 3)` and
/// `substr("hello", 0, 3)` are both `"hel"`. A non-positive `len` yields an empty string.
pub(crate) fn substr<'a>(base: &Str<'a>, pos: Int, len: Int) -> Str<'a> {
    if len <= 0 {
        return Str::default();
    }
    let start = pos.max(1) - 1;
    base.sub_str(start as usize, len as usize)
}

/// Pad `text` to `len` characters with the first char of `pad` (space if `pad` is empty);
/// text already `len` characters or longer is returned unchanged (use `truncate` to shorten). Lengths are counted in chars, not bytes; a
/// negative `len` is treated as 0, and one above [`MAX_RESULT_LEN`](crate::runtime::MAX_RESULT_LEN) is an error.
fn pad_text(func: &str, text: &str, len: Int, pad: &str, alignment: Alignment) -> Result<String> {
    check_result_len(func, "length", len as Float)?;
    let len = len.max(0) as usize;
    let char_count = text.chars().count();
    if char_count >= len {
        return Ok(text.to_string());
    }
    let pad_char = pad.chars().next().unwrap_or(' ');
    let fill = len - char_count;
    let (left, right) = match alignment {
        Alignment::Left => (0, fill),
        Alignment::Right => (fill, 0),
        Alignment::Middle => (fill / 2, fill - fill / 2),
        Alignment::MiddleRight => (fill - fill / 2, fill / 2),
    };
    let mut result = String::with_capacity(text.len() + fill * pad_char.len_utf8());
    result.extend(std::iter::repeat_n(pad_char, left));
    result.push_str(text);
    result.extend(std::iter::repeat_n(pad_char, right));
    Ok(result)
}

pub fn pad_left(text: &str, len: Int, pad: &str) -> Result<String> {
    pad_text("pad_end", text, len, pad, Alignment::Left)
}

pub fn pad_right(text: &str, len: Int, pad: &str) -> Result<String> {
    pad_text("pad_start", text, len, pad, Alignment::Right)
}

pub fn pad_both(text: &str, len: Int, pad: &str) -> Result<String> {
    pad_text("pad", text, len, pad, Alignment::MiddleRight)
}

/// The Jaro-Winkler similarity of `text1` and `text2`, from 0.0 (nothing in common) to 1.0 (the
/// same text); a common prefix counts more, which suits names and typos. Characters are compared;
/// bytes when a text is not valid UTF-8 (or with `-b`), so distinct invalid bytes stay distinct.
pub fn similar(text1: &[u8], text2: &[u8]) -> f64 {
    match (std::str::from_utf8(text1), std::str::from_utf8(text2)) {
        (Ok(a), Ok(b)) if !crate::runtime::str_impl::characters_as_bytes() => strsim::jaro_winkler(a, b),
        _ => strsim::generic_jaro_winkler::<ByteSeq, ByteSeq, u8, u8>(&ByteSeq(text1), &ByteSeq(text2)),
    }
}

/// The bytes of a text, as `strsim`'s generic functions take them.
struct ByteSeq<'a>(&'a [u8]);

impl<'a> IntoIterator for &ByteSeq<'a> {
    type Item = u8;
    type IntoIter = std::iter::Copied<std::slice::Iter<'a, u8>>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().copied()
    }
}

pub fn strcmp(text1: &[u8], text2: &[u8]) -> i64 {
    return if text1 == text2 {
        0
    } else if text1 < text2 {
        -1
    } else {
        1
    };
}

pub fn read_all(path: &str) -> String {
    let result = if path.starts_with("http://") || path.starts_with("https://") {
        crate::runtime::network::fetch_text(path)
    } else {
        std::fs::read_to_string(path).map_err(|e| e.to_string())
    };
    result.unwrap_or_else(|e| {
        stdlib_warning("read_all", format!("{}: {}", path, e));
        String::new()
    })
}

pub fn write_all(path: &str, content: &str) {
    if let Err(e) = std::fs::write(path, content) {
        stdlib_warning("write_all", format!("{}: {}", path, e));
    }
}

pub(crate) fn pairs<'a>(text: &str, pair_sep: &str, kv_sep: &str) -> StrMap<'a, Str<'a>> {
    let is_url_query = pair_sep == "&" && kv_sep == "=";
    let mut map = crate::runtime::HashMap::default();
    let text = text.trim_matches(|c| c == '"' || c == '\'');
    if is_url_query {
        // form_urlencoded decodes both key and value, and treats '+' as space
        for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
            if !value.is_empty() {
                map.insert(Str::from(key.into_owned()), Str::from(value.into_owned()));
            }
        }
    } else {
        for pair in text.split(pair_sep) {
            // split on the first kv_sep only, so values may contain kv_sep (e.g. base64 "abc==")
            if let Some((key, value)) = pair.split_once(kv_sep) {
                if !value.is_empty() {
                    map.insert(Str::from(key.to_string()), Str::from(value.to_string()));
                }
            }
        }
    }
    SharedMap::from(map)
}

use logos::Logos;
use regex::Regex;

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t\n\f]+")] // Ignore this regex pattern between tokens
enum RecordToken<'a> {
    #[token("{")]
    LBRACE,
    #[token("}")]
    RBRACE,
    #[token("(")]
    LPAREN,
    #[token(")")]
    RPAREN,
    #[token(",")]
    COMMA,
    #[token("=")]
    EQ,
    #[token(":")]
    COLON,
    #[token(";")]
    SEMICOLON,
    #[regex(r#"[a-zA-Z0-9_]+"#)]
    LITERAL(&'a str),
    #[regex(r#""[^"]*""#)]
    Text(&'a str),
    #[regex(r#"'[^']*'"#)]
    Text2(&'a str),
    #[regex(r#"(\d+)(\.\d+)?"#, priority = 3)]
    NUM(&'a str),
}

struct PairState {
    pub key: String,
    pub value: String,
    pub key_parsed: bool,
}

impl Default for PairState {
    fn default() -> Self {
        PairState {
            key: "".to_owned(),
            value: "".to_owned(),
            key_parsed: false,
        }
    }
}

impl PairState {
    pub fn reset(&mut self) {
        self.key = "".to_owned();
        self.value = "".to_owned();
        self.key_parsed = false;
    }

    pub fn is_legal(&self) -> bool {
        self.key != "" && self.value != ""
    }
}

/// parse message - `msg_name{key1=value1,key2=value2}(body)`
pub(crate) fn message<'b>(text: &str) -> StrMap<'b, Str<'b>> {
    record(text)
}

/// parse record: `attr_name{key1=value1,key2=value2}`
pub(crate) fn record<'b>(text: &str) -> StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    if text.starts_with('{') && text.ends_with('}') {
        // simple map
        let lexer = RecordToken::lexer(&text);
        let mut key_parsed = false;
        let mut key = "";
        let mut value = "";
        for token in lexer.into_iter() {
            if let Ok(attribute) = token {
                match attribute {
                    RecordToken::COLON | RecordToken::EQ => {
                        // key parsed
                        key_parsed = true
                    }
                    RecordToken::COMMA => {
                        // add pair
                        if !key.is_empty() && !value.is_empty() {
                            map.insert(Str::from(key.to_string()), Str::from(value.to_string()));
                        }
                        key = "";
                        value = "";
                        key_parsed = false
                    }
                    RecordToken::RBRACE => {
                        // end
                        if !key.is_empty() && !value.is_empty() {
                            map.insert(Str::from(key.to_string()), Str::from(value.to_string()));
                        }
                        key_parsed = false
                    }
                    // parse key's value
                    RecordToken::LITERAL(literal) => {
                        // pair value
                        if key_parsed {
                            value = literal;
                        } else {
                            key = literal;
                        }
                    }
                    RecordToken::Text(text) => {
                        // pair value
                        value = text.trim_matches('"');
                    }
                    RecordToken::Text2(text) => {
                        // pair value
                        value = text.trim_matches('\'');
                    }
                    RecordToken::NUM(num) => {
                        // pair value
                        value = num;
                    }
                    _ => {}
                }
            }
        }
    } else if text.contains("(") && text.ends_with(")") {
        // table definition: table_name(id integer, value double)
        let offset = text.find('(').unwrap();
        let name = text[0..offset].trim().to_string();
        if !name.is_empty() {
            map.insert(Str::from("_".to_owned()), Str::from(name));
        }
        let pairs_text = text[offset + 1..text.len() - 1].to_string();
        for pair in pairs_text.split(",") {
            let kv: Vec<&str> = pair.trim().split(" ").collect();
            if kv.len() == 2 {
                map.insert(
                    Str::from(kv[0].trim().to_string()),
                    Str::from(kv[1].trim().to_string()),
                );
            }
        }
    } else if text.contains('{') {
        // record_name{id=1,name="hello world"}
        let offset = text.find('{').unwrap();
        let name = text[0..offset].trim().to_string();
        if !name.is_empty() {
            map.insert(Str::from("_".to_owned()), Str::from(name));
        }
        let pairs_text = text[offset..].to_string();
        let mut pair_state = PairState::default();
        let mut body_started = false;
        let mut body = "".to_owned();
        let lexer = RecordToken::lexer(&pairs_text);
        for token in lexer.into_iter() {
            if let Ok(attribute) = token {
                match attribute {
                    RecordToken::COLON | RecordToken::EQ => {
                        // key parsed
                        pair_state.key_parsed = true;
                    }
                    RecordToken::LPAREN => {
                        // body started
                        body_started = true;
                    }
                    RecordToken::RPAREN => {
                        // boyd end
                        if !body.is_empty() {
                            map.insert(Str::from("_body".to_owned()), Str::from(body.clone()));
                        }
                        body_started = false;
                    }
                    // parse key's value
                    RecordToken::LITERAL(literal) if !body_started => {
                        // pair value
                        if pair_state.key_parsed {
                            pair_state.value = literal.to_string();
                            if pair_state.is_legal() {
                                map.insert(
                                    Str::from(pair_state.key.clone()),
                                    Str::from(pair_state.value.clone()),
                                );
                            }
                            pair_state.reset();
                        } else {
                            pair_state.key = literal.to_string();
                        }
                    }
                    RecordToken::Text(text) if !body_started => {
                        // pair value
                        pair_state.value = text[1..text.len() - 1].to_string();
                        if pair_state.is_legal() {
                            map.insert(
                                Str::from(pair_state.key.clone()),
                                Str::from(pair_state.value.clone()),
                            );
                        }
                        pair_state.reset();
                    }
                    RecordToken::Text2(text) if !body_started => {
                        // pair value
                        pair_state.value = text[1..text.len() - 1].to_string();
                        if pair_state.is_legal() {
                            map.insert(
                                Str::from(pair_state.key.clone()),
                                Str::from(pair_state.value.clone()),
                            );
                        }
                        pair_state.reset();
                    }
                    RecordToken::NUM(num) if !body_started => {
                        // pair value
                        pair_state.value = num.to_string();
                        if pair_state.is_legal() {
                            map.insert(
                                Str::from(pair_state.key.clone()),
                                Str::from(pair_state.value.clone()),
                            );
                        }
                        pair_state.reset();
                    }
                    // body value
                    RecordToken::LITERAL(literal) if body_started => {
                        body = literal.to_string();
                    }
                    RecordToken::NUM(num) if body_started => {
                        body = num.to_string();
                    }
                    RecordToken::Text(text) if body_started => {
                        body = text[1..text.len() - 1].to_string();
                    }
                    RecordToken::Text2(text) if body_started => {
                        body = text[1..text.len() - 1].to_string();
                    }
                    _ => {}
                }
            }
        }
    } else {
        map.insert(Str::from("_".to_owned()), Str::from(text.to_owned()));
    }
    SharedMap::from(map)
}

#[derive(Logos, Debug, PartialEq)]
#[logos(skip r"[ \t\n\f]+")] // Ignore this regex pattern between tokens
enum ParamsToken<'a> {
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

pub(crate) fn func<'a>(text: &str) -> IntMap<Str<'a>> {
    let result: IntMap<Str> = IntMap::default();
    if text.contains("(") {
        let offset = text.find('(').unwrap();
        let name = text[0..offset].trim().to_string();
        result.insert(0, Str::from(name));
        let params_text = text[offset..].to_string();
        let lexer = ParamsToken::lexer(&params_text);
        let mut index: i64 = 1;
        for token in lexer.into_iter() {
            if let Ok(attribute) = token {
                match attribute {
                    ParamsToken::LITERAL(literal) => {
                        result.insert(index, Str::from(literal.to_string()));
                        index = index + 1;
                    }
                    ParamsToken::Text(text) => {
                        result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                        index = index + 1;
                    }
                    ParamsToken::Text2(text) => {
                        result.insert(index, Str::from(text[1..text.len() - 1].to_string()));
                        index = index + 1;
                    }
                    ParamsToken::NUM(num) => {
                        result.insert(index, Str::from(num.to_string()));
                        index = index + 1;
                    }
                    _ => {}
                }
            }
        }
    }
    result
}

pub fn last_part(text: &str, sep: &str) -> String {
    if !sep.is_empty() {
        let parts: Vec<&str> = text.split(sep).collect();
        if parts.len() > 0 {
            return parts[parts.len() - 1].to_string();
        }
    } else {
        if text.contains('/') {
            return text
                .rfind('/')
                .map(|pos| &text[pos + 1..])
                .unwrap_or(text)
                .to_string();
        } else if text.contains('.') {
            return text
                .rfind('.')
                .map(|pos| &text[pos + 1..])
                .unwrap_or(text)
                .to_string();
        }
    }
    text.to_string()
}

/// extract {name} from template, and get matched value from text
/// for example: template = "hello {name}, welcome to {city}", text = "hello world, welcome to Beijing"
/// result is {"name": "world", "city": "Beijing"}
pub(crate) fn parse<'b>(text: &str, template: &str) -> StrMap<'b, Str<'b>> {
    let mut map = crate::runtime::HashMap::default();
    let mut tokens: Vec<String> = vec![];
    let mut names: Vec<String> = vec![];
    let mut sep: String = "".to_string();
    let mut name: String = "".to_string();
    let mut name_started = false;
    for c in template.chars() {
        if c == '{' && name_started || c == '}' && !name_started {
            stdlib_warning("parse", format!("unbalanced braces in template {:?}", template));
            return SharedMap::from(map);
        }
        if c == '{' {
            tokens.push(sep.to_string());
            sep.clear();
            name_started = true;
        } else if c == '}' {
            names.push(name.to_string());
            name.clear();
            name_started = false;
        } else if name_started {
            name.push(c);
        } else {
            sep.push(c);
        }
    }
    if name_started {
        stdlib_warning("parse", format!("unbalanced braces in template {:?}", template));
        return SharedMap::from(map);
    }
    tokens.push(sep.to_string());
    let mut sub_text = text;
    for i in 0..tokens.len() - 1 {
        let token = &tokens[i];
        let next_token = &tokens[i + 1];
        let name = &names[i];
        let start = if token.is_empty() {
            0
        } else if let Some(pos) = sub_text.find(token) {
            pos
        } else {
            break;
        };
        let offset = start + token.len();
        let remain = &sub_text[offset..];
        let end = if next_token.is_empty() {
            remain.len()
        } else {
            remain.find(next_token).unwrap_or(remain.len())
        };
        let value = &sub_text[offset..offset + end];
        map.insert(Str::from(name.to_string()), Str::from(value.to_string()));
        sub_text = &sub_text[offset..];
    }
    SharedMap::from(map)
}

pub(crate) fn rparse<'a>(text: &str, template: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let re = match Regex::new(template) {
        Ok(re) => re,
        Err(e) => {
            // the syntax error is a multi-line diagram ending with `error: <reason>`; keep the reason
            let e = e.to_string();
            let reason = e.lines().last().unwrap_or_default().trim_start_matches("error: ");
            stdlib_warning("rparse", format!("invalid regex {:?}: {}", template, reason));
            return map;
        }
    };
    if let Some(caps) = re.captures(text) {
        for i in 1..caps.len() {
            // an optional group that did not participate matches the empty string
            let group = caps.get(i).map_or("", |m| m.as_str());
            map.insert(i as i64, Str::from(group.to_string()));
        }
    }
    map
}

static PHONE_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[0-9+][0-9-]{5,16}").unwrap());

pub fn is_format(format: &str, text: &str) -> Int {
    let result = match format {
        "email" => email_address::EmailAddress::is_valid(text),
        "url" => {
            text.starts_with("http://")
                || text.starts_with("https://")
                || text.starts_with("ftp://")
        }
        "phone" => PHONE_REGEX.is_match(text),
        "ip" => {
            use std::net::{Ipv4Addr, Ipv6Addr};
            if text.contains(":") {
                text.parse::<Ipv6Addr>().is_ok()
            } else {
                text.parse::<Ipv4Addr>().is_ok()
            }
        }
        &_ => {
            stdlib_warning("is", format!("format not supported: {:?}", format));
            false
        }
    };
    if result {
        1
    } else {
        0
    }
}

/// Longest password `mkpass` generates; longer requests are errors rather than huge allocations.
const MAX_PASSWORD_LEN: usize = 4096;

pub fn generate_password(len: i64) -> Result<String> {
    if len <= 0 {
        stdlib_warning("mkpass", format!("length must be positive, got {}", len));
        return Ok(String::new());
    }
    let len = len as usize;
    if len > MAX_PASSWORD_LEN {
        return err!("mkpass: length {} is too large (maximum {})", len, MAX_PASSWORD_LEN);
    }
    let pg = passwords::PasswordGenerator {
        length: len,
        numbers: true,
        lowercase_letters: true,
        uppercase_letters: true,
        symbols: true,
        spaces: false,
        exclude_similar_characters: false,
        strict: true,
    };
    Ok(pg.generate_one().unwrap_or_else(|e| {
        stdlib_warning("mkpass", e);
        String::new()
    }))
}

pub fn figlet(text: &str) -> String {
    use figlet_rs::FIGlet;
    let standard_font = FIGlet::standard().unwrap();
    if let Some(figure) = standard_font.convert(text) {
        figure.to_string()
    } else {
        text.to_string()
    }
}

/// Word-wrap `text` into lines of at most `width` display columns (CJK and other wide chars
/// count as 2); a `width <= 0` means 80. Each existing line is handled on its own: a line that
/// fits is kept as is, otherwise it breaks at whitespace (runs of it collapse to a line break or
/// one space), and a word wider than `width` (e.g. CJK text without spaces) is split.
pub fn wrap(text: &str, width: Int) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    let width = if width <= 0 { 80 } else { usize::try_from(width).unwrap_or(usize::MAX) };
    let mut out = String::with_capacity(text.len() + text.len() / width + 1);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.width() <= width {
            out.push_str(line);
            continue;
        }
        let mut line_width = 0;
        for word in line.split_whitespace() {
            let word_width = word.width();
            if line_width > 0 && line_width + 1 + word_width <= width {
                out.push(' ');
                line_width += 1;
            } else if line_width > 0 {
                out.push('\n');
                line_width = 0;
            }
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                // Always place at least one char per line, even if it is wider than `width`.
                if line_width > 0 && line_width + w > width {
                    out.push('\n');
                    line_width = 0;
                }
                out.push(c);
                line_width += w;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn test_wrap() {
        assert_eq!(wrap("hello world", 5), "hello\nworld");
        assert_eq!(wrap("the quick brown fox", 10), "the quick\nbrown fox");
        assert_eq!(wrap("a  b   c d", 3), "a b\nc d");
        // Text that fits is returned as is, even with extra whitespace.
        assert_eq!(wrap("  hi  ", 10), "  hi  ");
        // Existing newlines are kept.
        assert_eq!(wrap("aa bb\ncc dd", 4), "aa\nbb\ncc\ndd");
        // Long words are split.
        assert_eq!(wrap("abcdefgh", 3), "abc\ndef\ngh");
        // Wide (CJK) chars take 2 columns.
        assert_eq!(wrap("你好世界你好", 4), "你好\n世界\n你好");
        assert_eq!(wrap("日本語日本語", 4), "日本\n語日\n本語");
        assert_eq!(wrap("ab日本", 3), "ab\n日\n本");
        // A char wider than `width` still goes on its own line.
        assert_eq!(wrap("日本", 1), "日\n本");
        // Each line is judged on its own: short lines keep their whitespace.
        assert_eq!(wrap("  keep  me\naa bb cc dd ee", 10), "  keep  me\naa bb cc\ndd ee");
        // A non-positive width means 80.
        let long = "word ".repeat(30);
        assert_eq!(wrap(&long, 0), wrap(&long, 80));
        assert_eq!(wrap(&long, -5).lines().map(|l| l.chars().count()).max(), Some(79));
    }

    #[test]
    fn test_substr() {
        let s = Str::from("hello");
        assert_eq!(substr(&s, 2, 3).as_str(), "ell");
        assert_eq!(substr(&s, 2, Int::MAX).as_str(), "ello");
        // A start before the first char starts at the first char, keeping the length.
        assert_eq!(substr(&s, -1, 3).as_str(), "hel");
        assert_eq!(substr(&s, 0, 1).as_str(), "h");
        assert_eq!(substr(&s, -10, 2).as_str(), "he");
        assert_eq!(substr(&s, -1, Int::MAX).as_str(), "hello");
        assert_eq!(substr(&s, 2, -1).as_str(), "");
        assert_eq!(substr(&s, 2, 0).as_str(), "");
        assert_eq!(substr(&s, 6, 1).as_str(), "");
        assert_eq!(substr(&Str::from("你好Hello"), 2, 2).as_str(), "好H");
        assert_eq!(substr(&Str::from("你好Hello"), -5, 2).as_str(), "你好");
    }

    #[test]
    fn test_parse() {
        let template = "{greet} {name}, welcome to {city}!";
        let text = "hello world, welcome to Beijing";
        let map = parse(text, template);
        println!("{:?}", map);
    }

    #[test]
    fn test_rparse() {
        let str = "ABC 01 D2E";
        let template = r"(\s)(\d+)(\s)";
        let map = rparse(str, template);
        println!("{:?}", map);
    }

    #[test]
    fn test_rparse_invalid_regex_sets_errno() {
        crate::runtime::set_errno(String::new());
        assert_eq!(rparse("abc", "([").len(), 0);
        assert_eq!(crate::runtime::errno(), r#"rparse: invalid regex "([": unclosed character class"#);
    }

    #[test]
    fn test_pad_left() {
        let text = pad_left("hello", 100, "*").unwrap();
        println!("{}", text);
    }

    #[test]
    fn test_pad_multibyte_and_empty_pad() {
        assert_eq!(pad_left("你好世界", 2, "*").unwrap(), "你好世界");
        assert_eq!(pad_right("abcdef", 2, "*").unwrap(), "abcdef");
        assert_eq!(pad_both("ab", 1, "*").unwrap(), "ab");
        assert_eq!(pad_right("ab", 5, "xy").unwrap(), "xxxab");
        assert_eq!(pad_left("你好", 4, "*").unwrap(), "你好**");
        assert_eq!(pad_right("你好", 4, "中").unwrap(), "中中你好");
        assert_eq!(pad_both("ab", 5, "*").unwrap(), "**ab*");
        assert_eq!(pad_left("ab", 4, "").unwrap(), "ab  ");
        assert_eq!(pad_right("abc", 3, "").unwrap(), "abc");
    }

    #[test]
    fn test_strcmp() {
        assert_eq!(strcmp(b"hello", b"Hello"), 1);
        assert_eq!(strcmp(b"caf\xe9", b"caf"), 1);
    }

    #[test]
    fn test_words() {
        let text = "Hello , world! could you give a 名称?";
        let words = text.unicode_words();
        for word in words {
            println!("{}", word);
        }
    }

    #[test]
    fn test_repeat() {
        let text = "12";
        let result = text.repeat(3);
        println!("{}", result);
    }

    #[test]
    fn test_read_all() {
        let content = read_all("demo.awk");
        println!("{}", content);
    }

    #[test]
    fn test_read_all_from_remote() {
        let content = read_all("https://httpbin.org/ip");
        println!("{}", content);
    }
    
    #[test]
    fn test_pairs() {
        let text = "name=hello;age=12";
        let map = pairs(text, ";", "=");
        println!("{:?}", map);
    }

    #[test]
    fn test_pairs_kv_sep_in_value() {
        let m = pairs("a=1&t=abc==&b=&q=a+b&n%20k=v", "&", "=");
        assert_eq!(m.get(&Str::from("t")).as_str(), "abc==");
        assert_eq!(m.get(&Str::from("q")).as_str(), "a b");
        assert_eq!(m.get(&Str::from("n k")).as_str(), "v");
        assert!(!m.contains(&Str::from("b")));
        let m = pairs("a:1;t:x:y", ";", ":");
        assert_eq!(m.get(&Str::from("t")).as_str(), "x:y");
    }

    #[test]
    fn test_struct() {
        let text = r#"{host: localhost, port: 1234, desc: 'demo demo'}"#;
        let map = record(text);
        println!("{}", map.get(&Str::from("desc")).as_str());
    }

    #[test]
    fn test_record() {
        let text = r#"mysql{host=localhost user=root password=123456 database=test}(1)"#;
        let map = record(text);
        println!("{}", map.get(&Str::from("_")).as_str());
        println!("{}", map.get(&Str::from("host")).as_str());
        println!("{}", map.get(&Str::from("user")).as_str());
        println!("body: {}", map.get(&Str::from("_body")).as_str());
    }

    #[test]
    fn test_table_record() {
        let text = r#"table1(id int, age int)"#;
        let map = record(text);
        println!("{}", map.get(&Str::from("_")).as_str());
        println!("{}", map.get(&Str::from("id")).as_str());
    }

    #[test]
    fn test_cookies() {
        let cookies_text = "_octo=GH1.1.178216615.1688558702; preferred_color_mode=light; tz=Asia%2FShanghai; _device_id=c49fdb13b5c41be361ee80236919ba50; user_session=qDSJ7GlA3aLriNnDG-KJsqw_QIFpmTBjt0vcLy5Vq2ay6StZ; __Host-user_session_same_site=qDSJ7GlA3aLriNnDG-KJsqw_QIFpmTBjt0vcLy5Vq2ay6StZ; tz=Asia%2FShanghai;";
        let cookies = pairs(cookies_text, ";", "=");
        println!("{}", cookies.get(&Str::from("_octo")).as_str());
        println!(
            "{}",
            cookies.get(&Str::from("preferred_color_mode")).as_str()
        );
        println!("{}", cookies.get(&Str::from("tz")).as_str());
    }

    #[test]
    fn test_complex_record() {
        let text = r#"http_requests_total{method="hello 你好 ' = : , world",code=200.01}"#;
        let map = record(text);
        println!("{}", map.get(&Str::from("method")).as_str());
        println!("{}", map.get(&Str::from("code")).as_str());
    }

    #[test]
    fn test_func() {
        let func_text = "hello(x,'hello world',11)";
        let map = func(func_text);
        let len = map.len();
        for i in 0..len {
            println!("{}", map.get(&(i as i64)).as_str());
        }
    }

    #[test]
    fn test_last_part() {
        let text = "demo/demo.txt";
        assert_eq!("demo.txt", last_part(text, ""));
    }

    #[test]
    fn test_is_format() {
        assert_eq!(1, is_format("phone", "008618667135137"));
    }

    #[test]
    fn test_generate_password() {
        println!("{}", generate_password(12).unwrap())
    }

    #[test]
    fn test_figlet() {
        println!("{}", figlet("Hello").trim_matches('\n'));
    }

    #[test]
    fn test_similar() {
        assert_eq!(similar(b"hello", b"hello"), 1.0);
        assert_eq!(similar(b"", b""), 1.0);
        assert_eq!(similar(b"abc", b""), 0.0);
        assert_eq!(similar(b"abc", b"xyz"), 0.0);
        assert!((similar(b"martha", b"marhta") - 0.9611).abs() < 1e-4);
        // a common prefix counts more: jaro-winkler
        assert!(similar(b"apple", b"applf") > similar(b"apple", b"bpple"));
        // characters, not bytes
        assert!((similar("你好世界".as_bytes(), "你好".as_bytes()) - 0.8667).abs() < 1e-4);
        // invalid UTF-8: the bytes
        assert_eq!(similar(b"caf\xe9x", b"caf\xe9x"), 1.0);
        assert!(similar(b"caf\xe9x", b"caf\xe8x") < 1.0);
    }
}
