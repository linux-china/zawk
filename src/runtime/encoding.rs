pub fn encode(format: &str, text: &str) -> String {
    return  format!("{}:{}", format, text);
}

pub fn decode(format: &str, text: &str) -> String {
    return "unbase64".to_string();
}