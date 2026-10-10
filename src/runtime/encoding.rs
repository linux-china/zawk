use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use base64::{engine::general_purpose::STANDARD, engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hashbrown::HashMap;
use urlencoding::{encode as url_encode, decode as url_decode};
use base58;
use base58::{FromBase58, ToBase58};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use flate2::read::{ZlibDecoder};
use growable_bloom_filter::{GrowableBloom, GrowableBloomBuilder};
use lazy_static::lazy_static;
use crate::runtime;
use crate::runtime::{stdlib_warning, SharedMap, Str};


pub fn encode(format: &str, text: &str) -> String {
    match format {
        "base32" => data_encoding::BASE32_NOPAD.encode(text.as_bytes()),
        "base32hex" => data_encoding::BASE32HEX_NOPAD.encode(text.as_bytes()),
        "base58" => text.as_bytes().to_base58(),
        "base62" => base62_encode(text.as_bytes()),
        "base64" => STANDARD.encode(text),
        "base85" => base85::encode(text.as_bytes()),
        "base64url" => URL_SAFE_NO_PAD.encode(text),
        "zlib2base64url" => {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
            e.write_all(text.as_bytes()).unwrap();
            let compressed_bytes = e.finish().unwrap();
            URL_SAFE_NO_PAD.encode(compressed_bytes)
        }
        "url" => url_encode(text).to_string(),
        "hex" => hex::encode(text),
        "hex-base64" => {
            match hex::decode(text) {
                Ok(bytes) => STANDARD.encode(&bytes),
                Err(e) => encode_failed(format, e),
            }
        }
        "hex-base64url" => {
            match hex::decode(text) {
                Ok(bytes) => URL_SAFE_NO_PAD.encode(&bytes),
                Err(e) => encode_failed(format, e),
            }
        }
        "base64-hex" => {
            match STANDARD.decode(text) {
                Ok(bytes) => hex::encode(&bytes),
                Err(e) => encode_failed(format, e),
            }
        }
        "base64url-hex" => {
            match URL_SAFE_NO_PAD.decode(text) {
                Ok(bytes) => hex::encode(&bytes),
                Err(e) => encode_failed(format, e),
            }
        }
        &_ => {
            format!("{}:{}", format, text)
        }
    }
}

fn encode_failed(format: &str, err: impl std::fmt::Display) -> String {
    stdlib_warning("encode", format!("{}: {}", format, err));
    String::new()
}

pub fn decode(format: &str, text: &str) -> String {
    if format == "base32" {
        if let Ok(bytes) = data_encoding::BASE32_NOPAD.decode(text.as_bytes()) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "base32hex" {
        if let Ok(bytes) = data_encoding::BASE32HEX_NOPAD.decode(text.as_bytes()) {
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
        if let Some(bytes) = base62_decode(text) {
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
    } else if format == "base85" {
        if let Ok(bytes) = base85::decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "base64url" {
        if let Ok(bytes) = URL_SAFE_NO_PAD.decode(text) {
            if let Ok(text) = String::from_utf8(bytes) {
                return text;
            }
        }
    } else if format == "zlib2base64url" {
        if let Ok(bytes) = URL_SAFE_NO_PAD.decode(text) {
            let mut d = ZlibDecoder::new(bytes.as_slice());
            let mut s = String::new();
            match d.read_to_string(&mut s) {
                Ok(_) => return s,
                Err(e) => stdlib_warning("decode", format!("{}: {}", format, e)),
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

lazy_static! {
    static ref BLOOM_FILTERS: Arc<Mutex<HashMap<String, GrowableBloom>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub fn bf_insert(item: &str, group: &str) {
    let mut filters = BLOOM_FILTERS.lock().unwrap();
    let filter = filters.entry(group.to_string()).or_insert_with(|| GrowableBloomBuilder::new().build());
    filter.insert(item);
}

pub fn bf_contains(item: &str, group: &str) -> i64 {
    let filters = BLOOM_FILTERS.lock().unwrap();
    if let Some(filter) = filters.get(group) {
        return filter.contains(item) as i64;
    }
    return 0;
}

pub fn bf_icontains(item: &str, group: &str) -> i64 {
    let mut filters = BLOOM_FILTERS.lock().unwrap();
    let filter = filters.entry(group.to_string()).or_insert_with(|| GrowableBloomBuilder::new().build());
    return if filter.contains(item) {
        1
    } else {
        filter.insert(item);
        0
    };
}

const BASE62_ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Base62 in the format of the `base-62` crate: the bytes, prefixed with `0x01` to keep leading
/// zeros, are a big-endian number written in base 62 with the least significant digit first.
fn base62_encode(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let mut num = Vec::with_capacity(bytes.len() + 1);
    num.push(1u8);
    num.extend_from_slice(bytes);
    let mut result = String::with_capacity(bytes.len() * 4 / 3 + 2);
    let mut start = 0;
    while start < num.len() {
        // divide num[start..] by 62 in place, keeping the remainder
        let mut rem = 0u32;
        for byte in &mut num[start..] {
            let acc = (rem << 8) | *byte as u32;
            *byte = (acc / 62) as u8;
            rem = acc % 62;
        }
        result.push(BASE62_ALPHABET[rem as usize] as char);
        while start < num.len() && num[start] == 0 {
            start += 1;
        }
    }
    result
}

fn base62_decode(text: &str) -> Option<Vec<u8>> {
    // big-endian bytes of the number, without leading zeros
    let mut num: Vec<u8> = Vec::with_capacity(text.len() * 3 / 4 + 1);
    for c in text.bytes().rev() {
        let digit = match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'Z' => c - b'A' + 10,
            b'a'..=b'z' => c - b'a' + 36,
            _ => return None,
        };
        // num = num * 62 + digit
        let mut carry = digit as u32;
        for byte in num.iter_mut().rev() {
            let acc = *byte as u32 * 62 + carry;
            *byte = acc as u8;
            carry = acc >> 8;
        }
        while carry > 0 {
            num.insert(0, carry as u8);
            carry >>= 8;
        }
    }
    // drop the 0x01 prefix
    if !num.is_empty() {
        num.remove(0);
    }
    Some(num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base62_roundtrip() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![0],
            vec![1],
            vec![0, 0],
            vec![0, 1],
            vec![1, 0],
            vec![0, 0, 0, 1],
            vec![62; 10],
            vec![63; 10],
            vec![0; 10],
            vec![255; 33],
            vec![0xDE, 0xAD, 0xBE, 0xEF],
        ];
        for input in cases {
            let encoded = base62_encode(&input);
            assert_eq!(base62_decode(&encoded), Some(input));
        }
        // values produced by the `base-62` crate
        assert_eq!(base62_encode(&[0xDE, 0xAD, 0xBE, 0xEF]), "JsoUl8");
        assert_eq!(
            base62_encode(b"Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt"),
            "Inj62xrWzFT5RgFoP72ZkfbrMabXdyZeYGijtTt8zuBN4XvHvEw6x2pk2BtdepGle57axcSeY2ixeXqOvwpE2VaEE3pHeeumHvIbZf0qUUxRBg99NrIALFCE"
        );
        assert_eq!(base62_decode("abc-"), None);
        assert_eq!(base62_decode("jSO+uL8"), None);
    }

    #[test]
    fn test_base32() {
        let text = "Hello";
        let encode_text = encode("base32", text);
        let plain = decode("base32", &encode_text);
        assert_eq!(&plain, text);
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

    #[test]
    fn test_zlib_base64() {
        let text = r#"@startuml
Bob -> Alice : hello
@enduml
        "#.trim();
        let encoded_text = encode("zlib2base64url", text);
        println!("encode: {}", encoded_text);
        let plain_text = decode("zlib2base64url", &encoded_text);
        println!("plain: {}", plain_text);
        assert_eq!(text, plain_text);
    }

    #[test]
    fn test_bf_insert() {
        bf_insert("first", "_");
        assert_eq!(bf_contains("first", "_"), 1);
    }

    #[test]
    fn test_icontains() {
        println!("first: {}", bf_icontains("first", "_"));
        println!("second: {}", bf_icontains("second", "_"));
        println!("first: {}", bf_icontains("first", "_"));
    }

    #[test]
    fn test_bloom_filter() {
        use growable_bloom_filter::GrowableBloomBuilder;
        let mut bloom = GrowableBloomBuilder::new().build();
        bloom.insert("first");
        assert!(bloom.contains("first"));
        assert_eq!(bloom.contains("second"), false);
    }

    #[test]
    fn test_base32hex() {
        let text = "Hello";
        let encoded_text = encode("base32hex", text);
        let plain_text = decode("base32hex", &encoded_text);
        assert_eq!(&plain_text, text);
    }

    #[test]
    fn test_base85() {
        let text = "Hello";
        let encoded_text = base85::encode(text.as_bytes());
        println!("{}", encoded_text);
        let bytes = base85::decode(&encoded_text).unwrap();
        let plain_text = String::from_utf8(bytes).unwrap();
        assert_eq!(plain_text, text);
    }
}
