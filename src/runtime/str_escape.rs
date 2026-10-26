use std::borrow::Cow;

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
            _ => result.push(c)
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
            _ => result.push(c)
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
    }

    #[test]
    fn test_shell_escape() {
        let text = "Hello ' world' hi world";
        println!("{}", escape_shell(text));
    }
}