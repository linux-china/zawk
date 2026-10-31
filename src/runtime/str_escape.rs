use std::borrow::Cow;
use std::sync::OnceLock;

/// The text of a NULL value, set by `--null`.
static NULL_TEXT: OnceLock<String> = OnceLock::new();

/// How a NULL value is written in a `-o tsv` field, as PostgreSQL `COPY` and MySQL `LOAD DATA`
/// read it. `-o tsv` escapes a backslash as `\\`, so no other field is written this way.
pub const TSV_NULL: &str = "\\N";

/// `--null text`: NULLs of database, JSON Lines and Parquet input are read as `text`, and a field
/// that is `text` is written as NULL by `-o tsv` (`\N`), `-o jsonl` and `-o parquet` (null).
pub fn set_null_text(text: String) {
    let _ = NULL_TEXT.set(text);
}

/// The `--null` text, if set.
#[inline]
pub fn null_text() -> Option<&'static str> {
    NULL_TEXT.get().map(String::as_str)
}

/// What a NULL value of the input reads as: the `--null` text, or the empty string.
pub fn null_input() -> &'static str {
    null_text().unwrap_or("")
}

/// Whether a field of the `-o tsv` stream is NULL: `\N` when `--null` is set.
pub fn is_tsv_null(raw: &[u8]) -> bool {
    null_text().is_some() && raw == TSV_NULL.as_bytes()
}

/// `isnull(s)`: 1 if `--null` is set and `s` is its text.
pub fn is_null(s: &[u8]) -> bool {
    null_text().is_some_and(|n| n.as_bytes() == s)
}

/// escape text by format; an unknown format is a warning and gives an empty string
pub fn escape(format: &str, text: &str) -> String {
    match format {
        "csv" => escape_csv(text),
        "tsv" => escape_tsv(text),
        "json" => escape_json(text),
        "sql" => escape_sql(text),
        "shell" => escape_shell(text),
        "xml" | "html" => escape_xml(text),
        _ => {
            crate::runtime::stdlib_warning("escape", format!("unsupported format {:?}", format));
            String::new()
        }
    }
}

/// RFC 4180: quote the field if needed and double any quotes; line breaks stay as is.
pub fn escape_csv(text: &str) -> String {
    if crate::runtime::splitter::batch::csv_needs_quotes(text.as_bytes()) {
        return format!("\"{}\"", text.replace('"', "\"\""));
    }
    text.to_string()
}

/// `-o tsv` escaping: a backslash, tab, newline or carriage return becomes `\\`, `\t`, `\n` or
/// `\r`, so that `-i tsv` reads the field back as is.
pub fn escape_tsv(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

/// Reverses [escape_tsv], as `-i tsv` does: `\t`, `\n`, `\r` and `\\` are a tab, a newline, a
/// carriage return and a backslash; any other backslash is kept.
pub fn unescape_tsv(text: &str) -> Cow<'_, str> {
    if !text.contains('\\') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    Cow::Owned(out)
}

pub fn escape_json(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\x08' => result.push_str("\\b"),
            '\x0c' => result.push_str("\\f"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            c if (c as u32) < 0x20 => result.push_str(&format!("\\u{:04x}", c as u32)),
            _ => result.push(c),
        }
    }
    return result;
}

pub fn escape_shell(text: &str) -> String {
    shell_escape::escape(Cow::from(text)).to_string()
}

pub fn escape_sql(text: &str) -> String {
    return text.replace("'", "''");
}

pub fn escape_xml(text: &str) -> String {
    let mut result = String::new();
    for c in text.chars() {
        match c {
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '&' => result.push_str("&amp;"),
            '"' => result.push_str("&quot;"),
            '\'' => result.push_str("&apos;"),
            _ => result.push(c),
        }
    }
    return result;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_json() {
        let json_text = "{\"id\": \n 1}";
        println!("{}", escape_json(json_text));
        assert_eq!(escape_json("a\x01b\x1f\x00"), "a\\u0001b\\u001f\\u0000");
        assert_eq!(escape_json("\"\\\n\t"), "\\\"\\\\\\n\\t");
    }

    #[test]
    fn test_shell_escape() {
        let text = "Hello ' world' hi world";
        println!("{}", escape_shell(text));
    }
}
