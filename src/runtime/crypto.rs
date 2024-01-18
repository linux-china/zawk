use sha2::{Sha256, Sha512, Digest};

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
    }
    format!("{}:{}", algorithm, text)
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
}