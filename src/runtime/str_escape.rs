/// escape text by format
pub fn escape(text: &str, format: &str) -> String {
    match format {
        "csv" => escape_csv(text),
        "json" => escape_json(text),
        "sql" => escape_sql(text),
        "xml" | "html" => escape_xml(text),
        _ => text.to_string()
    }
}

fn escape_csv(text: &str) -> String {
    if text.contains(",") || text.contains("\"") {
        return format!("\"{}\"", text.replace("\"", "\"\""));
    }
    return text.to_string();
}

fn escape_json(text: &str) -> String {
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

fn escape_sql(text: &str) -> String {
    return text.replace("'", "''");
}

fn escape_xml(text: &str) -> String {
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
}