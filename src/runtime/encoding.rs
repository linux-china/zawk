use base64::{engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE, Engine as _};
use urlencoding::{encode as url_encode, decode as url_decode};


pub fn encode(format: &str, text: &str) -> String {
    if format == "base64" {
        return STANDARD.encode(text);
    } else if format == "base64url" {
        return URL_SAFE.encode(text);
    } else if format == "url" {
        return url_encode(text).to_string();
    } else if format == "hex" {
        return hex::encode(text);
    } else if format == "hex-base64" {
        let bytes = hex::decode(text).unwrap();
        return STANDARD.encode(&bytes);
    } else if format == "hex-base64url" {
        let bytes = hex::decode(text).unwrap();
        return URL_SAFE.encode(&bytes);
    } else if format == "base64-hex" {
        let bytes = STANDARD.decode(text).unwrap();
        return hex::encode(&bytes);
    } else if format == "base64url-hex" {
        let bytes = URL_SAFE.decode(text).unwrap();
        return hex::encode(&bytes);
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
    } else if format == "base64url" {
        if let Ok(bytes) = URL_SAFE.decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "url" {
        if let Ok(url_text) = url_decode(text) {
            return url_text.to_string();
        }
    } else if format == "hex" {
        if let Ok(bytes) = hex::decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
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
        let plain_text = decode("base64", encoded_text);
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
        let plain_text = decode("url", encoded_text);
        println!("{}", plain_text);
        assert_eq!(plain_text, "Hello World")
    }

    #[test]
    fn test_hex2base64() {
        let base64_text = encode("hex-base64", "91e1fa4f7c75cfb9a684a2f54f7afdb10740c7177307ab227a618caffe993b05");
        println!("{}", base64_text);
    }
}