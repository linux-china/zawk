use crate::common::Result;
use crate::runtime::{Int, IntMap, SharedMap, Str, StrMap, check_result_len_int, stdlib_warning};
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
    check_result_len_int(func, "length", len)?;
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
        (Ok(a), Ok(b)) if !crate::runtime::str_impl::characters_as_bytes() => {
            strsim::jaro_winkler(a, b)
        }
        _ => strsim::generic_jaro_winkler::<ByteSeq, ByteSeq, u8, u8>(
            &ByteSeq(text1),
            &ByteSeq(text2),
        ),
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
    if text1 == text2 {
        0
    } else if text1 < text2 {
        -1
    } else {
        1
    }
}

pub fn read_all(path: &str) -> String {
    let result = if path.starts_with("http://") || path.starts_with("https://") {
        crate::runtime::network::fetch_text(path)
    } else if crate::runtime::s3::is_s3_url(path) {
        crate::runtime::s3::read_object_bytes(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .map_err(|e| e.to_string())
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
        // a bare key (`flag`) or an empty value (`b=`) maps to "", following URL query conventions
        for (key, value) in url::form_urlencoded::parse(text.as_bytes()) {
            map.insert(Str::from(key.into_owned()), Str::from(value.into_owned()));
        }
    } else {
        for pair in text.split(pair_sep) {
            if pair.trim().is_empty() {
                continue;
            }
            // split on the first kv_sep only, so values may contain kv_sep (e.g. base64 "abc==");
            // a pair without kv_sep is kept as a key with an empty value
            let (key, value) = pair.split_once(kv_sep).unwrap_or((pair, ""));
            // keys are trimmed so `a=1, b=2` yields `b`, not ` b`; values are kept as-is
            map.insert(
                Str::from(key.trim().to_string()),
                Str::from(value.to_string()),
            );
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
    LBrace,
    #[token("}")]
    RBrace,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token(",")]
    Comma,
    #[token("=")]
    Eq,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,
    #[regex(r#"[a-zA-Z0-9_]+"#)]
    Literal(&'a str),
    #[regex(r#""[^"]*""#)]
    Text(&'a str),
    #[regex(r#"'[^']*'"#)]
    Text2(&'a str),
    #[regex(r#"(\d+)(\.\d+)?"#, priority = 3)]
    Num(&'a str),
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
        !self.key.is_empty() && !self.value.is_empty()
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
        let lexer = RecordToken::lexer(text);
        let mut key_parsed = false;
        let mut key = "";
        let mut value = "";
        for attribute in lexer.flatten() {
            match attribute {
                RecordToken::Colon | RecordToken::Eq => {
                    // key parsed
                    key_parsed = true
                }
                RecordToken::Comma => {
                    // add pair
                    if !key.is_empty() && !value.is_empty() {
                        map.insert(Str::from(key.to_string()), Str::from(value.to_string()));
                    }
                    key = "";
                    value = "";
                    key_parsed = false
                }
                RecordToken::RBrace => {
                    // end
                    if !key.is_empty() && !value.is_empty() {
                        map.insert(Str::from(key.to_string()), Str::from(value.to_string()));
                    }
                    key_parsed = false
                }
                // parse key's value
                RecordToken::Literal(literal) => {
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
                RecordToken::Num(num) => {
                    // pair value
                    value = num;
                }
                _ => {}
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
        for attribute in lexer.flatten() {
            match attribute {
                RecordToken::Colon | RecordToken::Eq => {
                    // key parsed
                    pair_state.key_parsed = true;
                }
                RecordToken::LParen => {
                    // body started
                    body_started = true;
                }
                RecordToken::RParen => {
                    // boyd end
                    if !body.is_empty() {
                        map.insert(Str::from("_body".to_owned()), Str::from(body.clone()));
                    }
                    body_started = false;
                }
                // parse key's value
                RecordToken::Literal(literal) if !body_started => {
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
                RecordToken::Num(num) if !body_started => {
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
                RecordToken::Literal(literal) if body_started => {
                    body = literal.to_string();
                }
                RecordToken::Num(num) if body_started => {
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
    } else {
        map.insert(Str::from("_".to_owned()), Str::from(text.to_owned()));
    }
    SharedMap::from(map)
}

/// Split the items of a list literal such as `[1, -2.5, "a\"b", [2,3]]`
/// or `(x, 'y')` into an AWK array indexed from `start_index`.
///
/// * items are separated by commas and/or whitespace;
/// * `"..."` / `'...'` are quoted strings; backslash escapes (`\"`, `\'`,
///   `\\`, `\n`, `\t`, `\r`, `\0`) are decoded;
/// * a nested `[...]`, `(...)` or `{...}` is kept verbatim as one item;
/// * anything else (numbers with sign/exponent, identifiers, `true`) is a bare
///   item, kept as written.
///
/// The outer delimiter pair (`open`/`close`) is optional. Malformed input
/// (unterminated string or bracket, unexpected closing bracket, text after
/// the closing delimiter) is reported through `stdlib_warning`, and the items
/// parsed so far are returned.
pub(crate) fn parse_list_items<'a>(
    fn_name: &str,
    text: &str,
    open: char,
    close: char,
    result: &IntMap<Str<'a>>,
    start_index: Int,
) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let skip_ws = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    skip_ws(&mut i);
    let wrapped = i < chars.len() && chars[i] == open;
    if wrapped {
        i += 1;
    }
    let mut index = start_index;
    let mut closed = false;
    loop {
        // skip separators
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let c = chars[i];
        if wrapped && c == close {
            closed = true;
            i += 1;
            break;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let mut item = String::new();
            let mut terminated = false;
            while i < chars.len() {
                let ch = chars[i];
                if ch == '\\' && i + 1 < chars.len() {
                    let esc = chars[i + 1];
                    item.push(match esc {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        other => other,
                    });
                    i += 2;
                } else if ch == quote {
                    terminated = true;
                    i += 1;
                    break;
                } else {
                    item.push(ch);
                    i += 1;
                }
            }
            if !terminated {
                stdlib_warning(fn_name, format!("unterminated string in {:?}", text));
            }
            result.insert(index, Str::from(item));
            index += 1;
            if !terminated {
                return;
            }
        } else if c == '[' || c == '(' || c == '{' {
            // nested structure: keep the raw text as one item
            let begin = i;
            let mut stack = vec![c];
            i += 1;
            while i < chars.len() && !stack.is_empty() {
                let ch = chars[i];
                match ch {
                    '"' | '\'' => {
                        i += 1;
                        while i < chars.len() && chars[i] != ch {
                            if chars[i] == '\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                    }
                    '[' | '(' | '{' => stack.push(ch),
                    ']' | ')' | '}' => {
                        let expected = match stack.last() {
                            Some('[') => ']',
                            Some('(') => ')',
                            _ => '}',
                        };
                        if ch != expected {
                            break;
                        }
                        stack.pop();
                    }
                    _ => {}
                }
                i += 1;
            }
            let end = i.min(chars.len());
            if !stack.is_empty() {
                stdlib_warning(fn_name, format!("unbalanced brackets in {:?}", text));
                result.insert(
                    index,
                    Str::from(chars[begin..end].iter().collect::<String>()),
                );
                return;
            }
            result.insert(
                index,
                Str::from(chars[begin..end].iter().collect::<String>()),
            );
            index += 1;
        } else if c == ']' || c == ')' || c == '}' {
            stdlib_warning(fn_name, format!("unexpected '{}' in {:?}", c, text));
            return;
        } else {
            let begin = i;
            while i < chars.len() {
                let ch = chars[i];
                if ch.is_whitespace()
                    || ch == ','
                    || (wrapped && ch == close)
                    || matches!(ch, '[' | '(' | '{' | ']' | ')' | '}' | '"' | '\'')
                {
                    break;
                }
                i += 1;
            }
            result.insert(index, Str::from(chars[begin..i].iter().collect::<String>()));
            index += 1;
        }
    }
    if wrapped && !closed {
        stdlib_warning(fn_name, format!("missing '{}' in {:?}", close, text));
    } else if closed {
        skip_ws(&mut i);
        if i < chars.len() {
            stdlib_warning(
                fn_name,
                format!("unexpected text after '{}' in {:?}", close, text),
            );
        }
    }
}

pub(crate) fn func<'a>(text: &str) -> IntMap<Str<'a>> {
    let result: IntMap<Str> = IntMap::default();
    if let Some(offset) = text.find('(') {
        let name = text[0..offset].trim().to_string();
        result.insert(0, Str::from(name));
        parse_list_items("func", &text[offset..], '(', ')', &result, 1);
    }
    result
}

pub fn last_part(text: &str, sep: &str) -> String {
    if !sep.is_empty() {
        let parts: Vec<&str> = text.split(sep).collect();
        if !parts.is_empty() {
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
            stdlib_warning(
                "parse",
                format!("unbalanced braces in template {:?}", template),
            );
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
        stdlib_warning(
            "parse",
            format!("unbalanced braces in template {:?}", template),
        );
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

/// Upper bound (in characters) on both the text and the regex of `rparse`: very long inputs make
/// regex compilation/matching take seconds, and `rparse` is meant for parsing short strings.
pub(crate) const RPARSE_MAX_LEN: usize = 1024;

fn exceeds_chars(s: &str, max: usize) -> bool {
    // byte length is an upper bound of the char count, so only count chars when it may exceed
    s.len() > max && s.chars().count() > max
}

pub(crate) fn rparse<'a>(text: &str, template: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    if exceeds_chars(template, RPARSE_MAX_LEN) {
        stdlib_warning(
            "rparse",
            format!("regex longer than {} characters", RPARSE_MAX_LEN),
        );
        return map;
    }
    if exceeds_chars(text, RPARSE_MAX_LEN) {
        stdlib_warning(
            "rparse",
            format!("text longer than {} characters", RPARSE_MAX_LEN),
        );
        return map;
    }
    let re = match Regex::new(template) {
        Ok(re) => re,
        Err(e) => {
            // the syntax error is a multi-line diagram ending with `error: <reason>`; keep the reason
            let e = e.to_string();
            let reason = e
                .lines()
                .last()
                .unwrap_or_default()
                .trim_start_matches("error: ");
            stdlib_warning(
                "rparse",
                format!("invalid regex {:?}: {}", template, reason),
            );
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

/// Most compiled grok patterns kept per thread: a grok expression expands into a regex of
/// thousands of characters, so compiling it for every record would dominate the run time.
const GROK_CACHE_CAPACITY: usize = 64;

type GrokCompiled = std::rc::Rc<std::result::Result<grok::Pattern, String>>;

thread_local! {
    /// compiled patterns (or the compile error) keyed by (pattern, definitions)
    static GROK_CACHE: std::cell::RefCell<lru::LruCache<(String, String), GrokCompiled>> =
        std::cell::RefCell::new(lru::LruCache::new(
            std::num::NonZeroUsize::new(GROK_CACHE_CAPACITY).unwrap(),
        ));
}

/// The default grok pattern library, built once and cloned when custom definitions are added.
static GROK_DEFAULT: LazyLock<grok::Grok> = LazyLock::new(grok::Grok::with_default_patterns);

/// Parse custom definitions in the Logstash patterns file format: one `NAME regex` per line,
/// blank lines and `#` comments ignored.
fn grok_definitions(defs: &str) -> std::result::Result<Vec<(&str, &str)>, String> {
    let mut items = Vec::new();
    for line in defs.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.split_once(|c: char| c.is_whitespace()) {
            Some((name, regex)) if !regex.trim().is_empty() => {
                items.push((name, regex.trim()));
            }
            _ => {
                return Err(format!(
                    "invalid definition {:?}, expected `NAME regex`",
                    line
                ));
            }
        }
    }
    Ok(items)
}

fn grok_compile(pattern: &str, defs: &str) -> std::result::Result<grok::Pattern, String> {
    let compiled = if defs.trim().is_empty() {
        GROK_DEFAULT.compile(pattern, true)
    } else {
        let mut grok = GROK_DEFAULT.clone();
        for (name, regex) in grok_definitions(defs)? {
            grok.add_pattern(name, regex);
        }
        grok.compile(pattern, true)
    };
    compiled.map_err(|e| match e {
        grok::Error::DefinitionNotFound(name) => format!("unknown pattern %{{{}}}", name),
        grok::Error::RegexCompilationFailed(msg) => grok_regex_error(&msg),
        // the parser error comes as its Debug name, such as `InvalidPattern`
        grok::Error::GenericCompilationFailure(kind) => grok_syntax_error(&kind),
        grok::Error::RecursionTooDeep => {
            "patterns nested too deeply, or a pattern that refers to itself".to_string()
        }
        grok::Error::CompiledPatternIsEmpty(_) => "empty pattern".to_string(),
        e => e.to_string(),
    })
}

/// A readable reason for a malformed `%{...}`, from the Debug name of the grok parser error.
fn grok_syntax_error(kind: &str) -> String {
    const FORMS: &str = "expected %{NAME}, %{NAME:key} or %{NAME:key:type}";
    if let Some(c) = kind
        .strip_prefix("InvalidCharacter(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return format!(
            "invalid character {} in %{{...}}: a name may only use letters, digits and _, a key also - [ ] . ({})",
            c, FORMS
        );
    }
    match kind {
        "InvalidPattern" => format!(
            "unclosed %{{...}}, empty %{{}} or more than 3 parts ({})",
            FORMS
        ),
        "InvalidPatternDefinition" => format!(
            "malformed %{{...}}: empty key or type, or an inline %{{NAME=regex}} with braces or without the closing }} ({})",
            FORMS
        ),
        _ => format!("malformed pattern ({})", kind),
    }
}

/// A readable reason for a regex error. grok only gives the Debug form of the fancy-regex error,
/// followed by the whole expanded regex: compile that regex again to get the error itself, and
/// leave out its position, which is in the expanded regex the user never wrote.
fn grok_regex_error(msg: &str) -> String {
    let Some((_, regex)) = msg.split_once('\n') else {
        return msg.to_string();
    };
    let reason = match fancy_regex::Regex::new(regex) {
        Err(fancy_regex::Error::ParseError(_, e)) => e.to_string(),
        Err(fancy_regex::Error::CompileError(fancy_regex::CompileError::InnerError(e))) => {
            // a syntax error of the regex crate is a multi-line diagram ending with `error: <reason>`
            match e.syntax_error() {
                Some(e) => {
                    let e = e.to_string();
                    let last = e.lines().last().unwrap_or_default();
                    last.trim_start_matches("error: ").to_string()
                }
                None => e.to_string(),
            }
        }
        Err(fancy_regex::Error::CompileError(e)) => e.to_string(),
        Err(e) => e.to_string(),
        Ok(_) => return msg.lines().next().unwrap_or_default().to_string(),
    };
    let mut chars = reason.chars();
    let reason = match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => reason,
    };
    format!("invalid regex: {}", reason)
}

/// `grok(s, pattern[, defs])`: match a grok expression such as `%{IP:client} %{WORD:method}`
/// against `s`, and return the named (aliased) captures. Captures that did not participate in
/// the match are left out; a `:type` suffix (`%{NUMBER:bytes:int}`) only names the key `bytes`.
pub(crate) fn grok<'a>(text: &str, pattern: &str, defs: &str) -> StrMap<'a, Str<'a>> {
    let map: StrMap<Str> = StrMap::default();
    let key = (pattern.to_string(), defs.to_string());
    let compiled = GROK_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(compiled) = cache.get(&key) {
            return compiled.clone();
        }
        let compiled = std::rc::Rc::new(grok_compile(pattern, defs));
        cache.put(key, compiled.clone());
        compiled
    });
    match compiled.as_ref() {
        Ok(pattern) => {
            if let Some(matches) = pattern.match_against(text) {
                for (name, value) in matches.iter() {
                    map.insert(Str::from(name.to_string()), Str::from(value.to_string()));
                }
            }
        }
        Err(e) => stdlib_warning("grok", format!("invalid pattern {:?}: {}", pattern, e)),
    }
    map
}

static PHONE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9+][0-9-]{5,16}$").unwrap());

pub fn is_format(format: &str, text: &str) -> Int {
    let result = match format {
        "email" => email_address::EmailAddress::is_valid(text),
        "url" => url::Url::parse(text).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https" | "ftp")
                && u.host_str().is_some_and(|h| !h.is_empty())
        }),
        "phone" => PHONE_REGEX.is_match(text),
        "ip" => text.parse::<std::net::IpAddr>().is_ok(),
        "ipv4" => text.parse::<std::net::Ipv4Addr>().is_ok(),
        "ipv6" => text.parse::<std::net::Ipv6Addr>().is_ok(),
        "date" => is_date(text),
        "uuid" => uuid::Uuid::try_parse(text).is_ok(),
        "json" => serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok(),
        "idcard_cn" => is_idcard_cn(text),
        &_ => {
            stdlib_warning("is", format!("format not supported: {:?}", format));
            false
        }
    };
    if result { 1 } else { 0 }
}

static DATE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}(?:[ T][0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]+)?(Z|[+-][0-9]{2}:?[0-9]{2})?)?$").unwrap()
});

/// A real calendar date in ISO form, optionally with a time: `2024-02-29`, `2024-02-29 13:45:00`,
/// `2024-02-29T13:45:00.123` or with an offset (`Z`, `+08:00`, `+0800`). Fields must be zero-padded
/// and unsigned, with no surrounding whitespace. Relative words such as `now`, which `mktime`
/// accepts, are not dates here.
fn is_date(text: &str) -> bool {
    use chrono::{NaiveDate, NaiveTime};
    let Some(caps) = DATE_REGEX.captures(text) else {
        return false;
    };
    if NaiveDate::parse_from_str(&text[..10], "%Y-%m-%d").is_err() {
        return false;
    }
    if text.len() == 10 {
        return true;
    }
    let time_end = caps.get(1).map_or(text.len(), |m| m.start());
    if NaiveTime::parse_from_str(&text[11..time_end], "%H:%M:%S%.f").is_err() {
        return false;
    }
    match caps.get(1).map(|m| m.as_str()) {
        None | Some("Z") => true,
        Some(offset) => {
            let digits = offset[1..].replace(':', "");
            digits[..2].parse::<u32>().is_ok_and(|h| h <= 23)
                && digits[2..].parse::<u32>().is_ok_and(|m| m <= 59)
        }
    }
}

/// Chinese resident identity card number (GB 11643): 17 digits with a valid birth date at 7..15,
/// then the ISO 7064 MOD 11-2 check character (`0`-`9` or `X`).
fn is_idcard_cn(text: &str) -> bool {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECK_CODES: &[u8; 11] = b"10X98765432";
    let bytes = text.as_bytes();
    if bytes.len() != 18 || !bytes[..17].iter().all(u8::is_ascii_digit) {
        return false;
    }
    if chrono::NaiveDate::parse_from_str(&text[6..14], "%Y%m%d").is_err() {
        return false;
    }
    let sum: u32 = bytes[..17]
        .iter()
        .zip(WEIGHTS)
        .map(|(b, w)| (b - b'0') as u32 * w)
        .sum();
    bytes[17].to_ascii_uppercase() == CHECK_CODES[(sum % 11) as usize]
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
        return err!(
            "mkpass: length {} is too large (maximum {})",
            len,
            MAX_PASSWORD_LEN
        );
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
/// count as 2) with the `textwrap` crate; a `width <= 0` means 80. Each existing line is handled
/// on its own: a line that fits is kept as is, otherwise runs of whitespace collapse to one space
/// and the line breaks at Unicode line-break opportunities (spaces, between CJK chars, after
/// hyphens), and a word wider than `width` is split.
pub fn wrap(text: &str, width: Int) -> String {
    use textwrap::{Options, WordSeparator};
    use unicode_width::UnicodeWidthStr;
    let width = if width <= 0 {
        80
    } else {
        usize::try_from(width).unwrap_or(usize::MAX)
    };
    let options = Options::new(width)
        .word_separator(WordSeparator::UnicodeBreakProperties)
        .break_words(true);
    let mut out = String::with_capacity(text.len() + text.len() / width + 1);
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line.width() <= width {
            out.push_str(line);
            continue;
        }
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&textwrap::fill(&normalized, &options));
    }
    out
}

/// Apply a terminal color and/or styles to `text`, as `color("red+bold", s)`.
/// `spec` holds tokens separated by `+` or whitespace, in any order and case-insensitive:
/// colors `red`, `green`, `blue`, `yellow`, `magenta`, `cyan`, `white`, `black`, and styles
/// `bold`, `underline`, `italic`, `dimmed`, `reversed`, `blink`, `hidden`, `strikethrough`.
/// Unknown tokens are ignored, so an unknown spec returns `text` unchanged.
///
/// `color()` returns a string and cannot know where that string will be written, so whether
/// escape codes are emitted is decided once for the whole process, as with the `colored`
/// crate: only when the process's stdout is a terminal and `NO_COLOR` is not set
/// (`CLICOLOR_FORCE=1` forces them). With a terminal stdout, `print color(..) > "file"`,
/// `| "cmd"` or a string built with `sprintf` still holds escape codes.
pub fn color(spec: &str, text: &str) -> String {
    color_with(
        spec,
        text,
        colored::control::SHOULD_COLORIZE.should_colorize(),
    )
}

/// Like [`color`], but `enabled` decides whether escape codes are emitted instead of the
/// process-wide terminal detection.
pub fn color_with(spec: &str, text: &str, enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    // SGR codes: styles in ascending order, then the foreground color (the last one wins).
    let mut styles = [false; 10];
    let mut fg: Option<u8> = None;
    for token in spec.split(|c: char| c == '+' || c.is_whitespace()) {
        match token.to_ascii_lowercase().as_str() {
            "black" => fg = Some(30),
            "red" => fg = Some(31),
            "green" => fg = Some(32),
            "yellow" => fg = Some(33),
            "blue" => fg = Some(34),
            "magenta" => fg = Some(35),
            "cyan" => fg = Some(36),
            "white" => fg = Some(37),
            "bold" => styles[1] = true,
            "dimmed" => styles[2] = true,
            "italic" => styles[3] = true,
            "underline" => styles[4] = true,
            "blink" => styles[5] = true,
            "reversed" => styles[7] = true,
            "hidden" => styles[8] = true,
            "strikethrough" => styles[9] = true,
            _ => {}
        }
    }
    let codes: Vec<String> = (0u8..10)
        .filter(|&i| styles[i as usize])
        .chain(fg)
        .map(|c| c.to_string())
        .collect();
    if codes.is_empty() {
        return text.to_string();
    }
    let prefix = format!("\x1b[{}m", codes.join(";"));
    // Re-apply this style after a reset inside `text`, so nested `color()` calls work.
    let body = text.replace("\x1b[0m", &format!("\x1b[0m{prefix}"));
    format!("{prefix}{body}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    /// Every pattern of the built-in library (`grok::patterns()`, the one
    /// `Grok::with_default_patterns` loads) can be used by name with the fancy-regex engine.
    #[test]
    fn test_grok_default_patterns_compile() {
        let names: Vec<&str> = grok::patterns().iter().map(|(name, _)| *name).collect();
        assert!(names.len() > 300, "{} patterns", names.len());
        let failed: Vec<String> = names
            .iter()
            .filter_map(|name| {
                grok_compile(&format!("%{{{}:v}}", name), "")
                    .err()
                    .map(|e| format!("{}: {}", name, e))
            })
            .collect();
        assert!(failed.is_empty(), "{:#?}", failed);
    }

    #[test]
    fn test_color() {
        // `color_with` takes the on/off switch explicitly, so this test does not touch the
        // process-wide `colored::control` override other tests could depend on.
        let c = |spec: &str, text: &str| color_with(spec, text, true);
        assert_eq!(c("red", "hi"), "\x1b[31mhi\x1b[0m");
        assert_eq!(c("red+bold", "hi"), "\x1b[1;31mhi\x1b[0m");
        assert_eq!(c("bold red", "hi"), c("red+bold", "hi"));
        assert_eq!(c(" Bold + RED ", "hi"), c("red bold", "hi"));
        assert_eq!(c("green underline", "hi"), "\x1b[4;32mhi\x1b[0m");
        assert_eq!(c("bold+bold", "hi"), "\x1b[1mhi\x1b[0m");
        // Unknown colors and styles are ignored.
        assert_eq!(c("pink", "hi"), "hi");
        assert_eq!(c("", "hi"), "hi");
        assert_eq!(c("pink+bold", "hi"), c("bold", "hi"));
        assert_eq!(c("red+shiny", "hi"), c("red", "hi"));
        assert_eq!(color_with("red+bold", "hi", false), "hi");
        // Nested: the outer style comes back after the inner reset.
        assert_eq!(
            c("bold", &c("red", "a")),
            "\x1b[1m\x1b[31ma\x1b[0m\x1b[1m\x1b[0m"
        );
    }

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
        assert_eq!(
            wrap("  keep  me\naa bb cc dd ee", 10),
            "  keep  me\naa bb cc\ndd ee"
        );
        // Lines break after hyphens and between CJK chars in mixed text.
        assert_eq!(wrap("well-known words", 8), "well-\nknown\nwords");
        assert_eq!(wrap("hi 你好世界", 6), "hi 你\n好世界");
        // A non-positive width means 80.
        let long = "word ".repeat(30);
        assert_eq!(wrap(&long, 0), wrap(&long, 80));
        assert_eq!(
            wrap(&long, -5).lines().map(|l| l.chars().count()).max(),
            Some(79)
        );
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
        assert_eq!(
            crate::runtime::errno(),
            r#"rparse: invalid regex "([": unclosed character class"#
        );
    }

    #[test]
    fn test_rparse_length_limit() {
        let long = "ab".repeat(70000);
        crate::runtime::set_errno(String::new());
        assert_eq!(rparse(&long, "(ab)").len(), 0);
        assert_eq!(
            crate::runtime::errno(),
            "rparse: text longer than 1024 characters"
        );
        crate::runtime::set_errno(String::new());
        assert_eq!(rparse("ab", &long).len(), 0);
        assert_eq!(
            crate::runtime::errno(),
            "rparse: regex longer than 1024 characters"
        );
        // exactly at the limit (multi-byte chars count as one) still works
        let text = "你".repeat(1024);
        assert_eq!(rparse(&text, "^(你+)$").len(), 1);
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
    #[ignore]
    fn test_read_all_from_s3() {
        dotenvy::dotenv().ok();
        let content = read_all("s3://mj-artifacts/demo.csv");
        assert!(content.starts_with("id,name,age"));
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
        assert!(m.contains(&Str::from("b")));
        assert_eq!(m.get(&Str::from("b")).as_str(), "");
        let m = pairs("a:1;t:x:y", ";", ":");
        assert_eq!(m.get(&Str::from("t")).as_str(), "x:y");
    }

    #[test]
    fn test_pairs_trim_key() {
        let m = pairs("a=1, b=2", ",", "=");
        assert!(m.contains(&Str::from("b")));
        assert!(!m.contains(&Str::from(" b")));
        assert_eq!(m.get(&Str::from("b")).as_str(), "2");
    }

    #[test]
    fn test_pairs_bare_key() {
        let m = pairs("a=1&flag&&b=2", "&", "=");
        assert_eq!(m.len(), 3);
        assert!(m.contains(&Str::from("flag")));
        assert_eq!(m.get(&Str::from("flag")).as_str(), "");
        let m = pairs("a=1,flag,,b=,", ",", "=");
        assert_eq!(m.len(), 3);
        assert!(m.contains(&Str::from("flag")));
        assert!(m.contains(&Str::from("b")));
        assert_eq!(m.get(&Str::from("a")).as_str(), "1");
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
        let map = func("hello(x,'hello world',11)");
        assert_eq!(map.len(), 4);
        assert_eq!(map.get(&0).as_str(), "hello");
        assert_eq!(map.get(&1).as_str(), "x");
        assert_eq!(map.get(&2).as_str(), "hello world");
        assert_eq!(map.get(&3).as_str(), "11");
        let map = func(r#"f(-1, -2.5e3, "a\"b", [1,2])"#);
        assert_eq!(map.get(&1).as_str(), "-1");
        assert_eq!(map.get(&2).as_str(), "-2.5e3");
        assert_eq!(map.get(&3).as_str(), "a\"b");
        assert_eq!(map.get(&4).as_str(), "[1,2]");
        assert_eq!(func("f()").len(), 1);
        assert_eq!(func("noparens").len(), 0);
    }

    #[test]
    fn test_last_part() {
        let text = "demo/demo.txt";
        assert_eq!("demo.txt", last_part(text, ""));
    }

    #[test]
    fn test_is_format() {
        assert_eq!(1, is_format("phone", "008618667135137"));
        assert_eq!(1, is_format("ip", "::1"));
        assert_eq!(1, is_format("ipv4", "192.168.1.1"));
        assert_eq!(0, is_format("ipv4", "::1"));
        assert_eq!(0, is_format("ipv4", "256.1.1.1"));
        assert_eq!(1, is_format("ipv6", "fe80::1"));
        assert_eq!(0, is_format("ipv6", "192.168.1.1"));
        assert_eq!(1, is_format("date", "2024-02-29"));
        assert_eq!(1, is_format("date", "2024-02-29 13:45:00"));
        assert_eq!(1, is_format("date", "2024-02-29T13:45:00.5+08:00"));
        assert_eq!(0, is_format("date", "2023-02-29"));
        assert_eq!(0, is_format("date", "now"));
        assert_eq!(1, is_format("date", "2024-02-29T13:45:00Z"));
        assert_eq!(1, is_format("date", "2024-02-29 13:45:00+0800"));
        assert_eq!(0, is_format("date", "2024-1-5"));
        assert_eq!(0, is_format("date", " 2024-01-05"));
        assert_eq!(0, is_format("date", "+2024-01-05"));
        assert_eq!(0, is_format("date", "2024-01-05 1:2:3"));
        assert_eq!(0, is_format("date", "2024-01-05 24:00:00"));
        assert_eq!(0, is_format("date", "2024-01-05T13:45:00+24:00"));
        assert_eq!(0, is_format("date", "2024-01-05T13:45"));
        assert_eq!(1, is_format("uuid", "67e55044-10b1-426f-9247-bb680e5fe0c8"));
        assert_eq!(0, is_format("uuid", "67e55044-10b1-426f-9247"));
        assert_eq!(1, is_format("uuid", "67e5504410b1426f9247bb680e5fe0c8"));
        assert_eq!(0, is_format("uuid", "6-7e55044-10b1-426f-9247bb680e5fe0c8"));
        assert_eq!(
            0,
            is_format("uuid", "67e55044-10b1-426f-9247-bb680e5fe0c8x")
        );
        assert_eq!(0, is_format("phone", "abc12345678"));
        assert_eq!(0, is_format("phone", "12345678abc"));
        assert_eq!(1, is_format("phone", "+86-186-6713-5137"));
        assert_eq!(1, is_format("url", "https://example.com/a?b=1"));
        assert_eq!(1, is_format("url", "ftp://127.0.0.1/pub"));
        assert_eq!(0, is_format("url", "http://"));
        assert_eq!(0, is_format("url", "https://"));
        assert_eq!(0, is_format("url", "mailto:a@b.com"));
        assert_eq!(0, is_format("url", "see http://example.com"));
        assert_eq!(1, is_format("json", r#"{"a": [1, 2]}"#));
        assert_eq!(0, is_format("json", "{a: 1}"));
        assert_eq!(1, is_format("idcard_cn", "11010519491231002X"));
        assert_eq!(1, is_format("idcard_cn", "11010519491231002x"));
        assert_eq!(0, is_format("idcard_cn", "110105194912310021"));
        assert_eq!(0, is_format("idcard_cn", "110105194913310028"));
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
