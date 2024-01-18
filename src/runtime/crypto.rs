use std::io::Cursor;
use sha2::{Sha256, Sha512, Digest};
use hmac::{Hmac, Mac};

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

/// Message Digest with md5, sha256, sha512
pub fn digest(algorithm: &str, text: &str) -> String {
    if algorithm == "md5" || algorithm == "md-5" {
        return format!("{:x}", md5::compute(text));
    } else if algorithm == "sha256" || algorithm == "sha-256" {
        let mut hasher = Sha256::default();
        hasher.update(text.as_bytes());
        return format!("{:x}", hasher.finalize());
    } else if algorithm == "sha512" || algorithm == "sha-512" {
        let mut hasher = Sha512::default();
        hasher.update(text.as_bytes());
        return format!("{:x}", hasher.finalize());
    } else if algorithm == "bcrypt" {
        return bcrypt::hash(text, bcrypt::DEFAULT_COST).unwrap();
    } else if algorithm == "murmur3" {
        let hashcode = murmur3::murmur3_32(&mut Cursor::new(text), 0).unwrap();
        return hashcode.to_string();
    }
    format!("{}:{}", algorithm, text)
}

/// HMAC(Hash-based message authentication code) with HmacSHA256 and HmacSHA512
pub fn hmac(algorithm: &str, key: &str, text: &str) -> String {
    return if algorithm == "HmacSHA512" {
        let mut mac = HmacSha512::new_from_slice(key.as_bytes()).unwrap();
        mac.update(text.as_bytes());
        format!("{:x}", mac.finalize().into_bytes())
    } else {
        let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
        mac.update(text.as_bytes());
        format!("{:x}", mac.finalize().into_bytes())
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5() {
        let digest_message = digest("md5", "hello");
        println!("{}", digest_message);
    }

    #[test]
    fn test_sha_256() {
        let digest_message = digest("sha256", "hello");
        println!("{}", digest_message);
    }

    #[test]
    fn test_sha_512() {
        let digest_message = digest("sha512", "hello");
        println!("{}", digest_message);
    }

    #[test]
    fn test_hmac_sha_256() {
        let signature = hmac("HmacSha256", "7f4ebc75-7476-453e-b8d2-bebe17352b0a", "hello");
        println!("{}", signature);
    }

    #[test]
    fn test_murmur3() {
        use std::io::Cursor;
        let hash_result = murmur3::murmur3_32(&mut Cursor::new("Hello"), 0);
        println!("{}", hash_result.unwrap());
    }
}