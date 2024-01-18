use base64::{engine::general_purpose::STANDARD, Engine as _};
use urlencoding::{encode as url_encode, decode as url_decode};


pub fn encode(format: &str, text: &str) -> String {
    if format == "base64" {
        return STANDARD.encode(text);
    } else if format == "url" {
        return url_encode(text).to_string();
    }
    return format!("{}:{}", format, text);
}

pub fn decode(format: &str, text: &str) -> String {
    if format == "base64" {
        if let Ok(bytes) = STANDARD.decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "url" {
        if let Ok(url_text) = url_decode(text) {
            return url_text.to_string();
        }
    }
    return format!("{}:{}", format, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64() {
        let encode_text = encode("base64", "Hello");
        println!("{}", encode_text);
        assert_eq!(encode_text, "SGVsbG8=")
    }

    #[test]
    fn test_un_base64() {
        let encoded_text = "SGVsbG8=";
        let plain_text = decode("base64", "SGVsbG8=");
        println!("{}", plain_text);
        assert_eq!(plain_text, "Hello")
    }

    #[test]
    fn test_url_encode() {
        let encode_text = encode("url", "Hello World");
        println!("{}", encode_text);
        assert_eq!(encode_text, "Hello%20World")
    }

    #[test]
    fn test_url_decode() {
        let encoded_text = "Hello%20World";
        let plain_text = decode("url", "Hello%20World");
        println!("{}", plain_text);
        assert_eq!(plain_text, "Hello World")
    }
}