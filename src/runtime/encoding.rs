use base64::{engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE, Engine as _};
use hashbrown::HashMap;
use urlencoding::{encode as url_encode, decode as url_decode};
use base58;
use base58::{FromBase58, ToBase58};
use crate::runtime;
use crate::runtime::{SharedMap, Str};


pub fn encode(format: &str, text: &str) -> String {
    match format {
        "base32" => base32::encode(base32::Alphabet::RFC4648 { padding: false }, text.as_bytes()),
        "base58" => text.as_bytes().to_base58(),
        "base62" => base_62::encode(text.as_bytes()),
        "base64" => STANDARD.encode(text),
        "base64url" => URL_SAFE.encode(text),
        "url" => url_encode(text).to_string(),
        "hex" => hex::encode(text),
        "hex-base64" => {
            let bytes = hex::decode(text).unwrap();
            STANDARD.encode(&bytes)
        }
        "hex-base64url" => {
            let bytes = hex::decode(text).unwrap();
            URL_SAFE.encode(&bytes)
        }
        "base64-hex" => {
            let bytes = STANDARD.decode(text).unwrap();
            hex::encode(&bytes)
        }
        "base64url-hex" => {
            let bytes = URL_SAFE.decode(text).unwrap();
            hex::encode(&bytes)
        }
        &_ => {
            format!("{}:{}", format, text)
        }
    }
}

pub fn decode(format: &str, text: &str) -> String {
    if format == "base32" {
        if let Some(bytes) = base32::decode(base32::Alphabet::RFC4648 { padding: false }, text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "base58" {
        if let Ok(bytes) = text.from_base58() {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "base62" {
        if let Ok(bytes) = base_62::decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "base64" {
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

pub(crate) fn data_url<'b>(text: &str) -> runtime::StrMap<'b, Str<'b>> {
    let mut map: HashMap<Str, Str> = HashMap::new();
    if text.starts_with("data:") {
        let parts: Vec<&str> = text.splitn(2, ",").collect();
        if parts.len() == 2 {
            let data = parts[1];
            map.insert(Str::from("data"), Str::from(data.trim().to_string()));
            let metadata: Vec<&str> = parts[0][5..].splitn(2, ";").collect();
            if metadata.len() == 2 {
                let mime_type = metadata[0];
                let encoding = metadata[1];
                map.insert(Str::from("mime_type"), Str::from(mime_type.to_string()));
                map.insert(Str::from("encoding"), Str::from(encoding.to_string()));
            } else {
                let mime_type = metadata[0];
                map.insert(Str::from("mime_type"), Str::from(mime_type.to_string()));
            }
        }
    }
    return SharedMap::from(map);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base32() {
        let encode_text = encode("base32", "Hello");
        println!("{}", encode_text);
    }

    #[test]
    fn test_base62() {
        let text = "Hello";
        let encoded_text = encode("base62", text);
        assert_eq!(encoded_text, "Rs8MZpO");
        let text2 = decode("base62", &encoded_text);
        assert_eq!(text2, text);
    }

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

    #[test]
    fn test_data_url() {
        let text = "data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==";
        let text2 = "data:text/html,%3Ch1%3EHello%2C%20World%21%3C%2Fh1%3E";
        let map = data_url(text);
        let map2 = data_url(text2);
        println!("{}", map.get(&Str::from("encoding")).as_str());
        println!("{}", map2.get(&Str::from("mime_type")).as_str());
    }
}