use std::collections::{BTreeMap, HashMap};
use std::io::{BufReader, Cursor};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use sha2::{Sha256, Sha512, Digest};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Number, Value};
use aes::cipher::{block_padding::Pkcs7, BlockModeDecrypt, BlockModeEncrypt, KeyIvInit};
use aes::cipher::consts::U12;
use base64::{Engine, engine::general_purpose::STANDARD};
use jsonwebtoken::{Algorithm, Header, DecodingKey, EncodingKey, Validation, decode_header};
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, JwkSet};
use lazy_static::lazy_static;
use crate::runtime::{SharedMap, Str, StrMap};

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;
type Aes128CbcEnc = cbc::Encryptor<aes::Aes128Enc>;
type Aes256CbcEnc = cbc::Encryptor<aes::Aes256Enc>;
type Aes128CbcDec = cbc::Decryptor<aes::Aes128Dec>;
type Aes256CbcDec = cbc::Decryptor<aes::Aes256Dec>;

/// Message Digest with md5, sha256, sha512
pub fn digest(algorithm: &str, text: &str) -> String {
    if algorithm == "md5" || algorithm == "md-5" {
        return format!("{:x}", md5::compute(text));
    } else if algorithm == "adler32" {
        return adler::adler32(BufReader::new(text.as_bytes())).unwrap().to_string();
    } else if algorithm == "crc32" {
        return crc::Crc::<u32>::new(&crc::CRC_32_CKSUM).checksum(text.as_bytes()).to_string();
    } else if algorithm == "blake3" {
        return blake3::hash(text.as_bytes()).to_string();
    } else if algorithm == "sha256" || algorithm == "sha-256" {
        let mut hasher = Sha256::default();
        hasher.update(text.as_bytes());
        return hex::encode(hasher.finalize());
    } else if algorithm == "sha512" || algorithm == "sha-512" {
        let mut hasher = Sha512::default();
        hasher.update(text.as_bytes());
        return hex::encode(hasher.finalize());
    } else if algorithm == "bcrypt" {
        return bcrypt::hash(text, bcrypt::DEFAULT_COST).unwrap();
    } else if algorithm == "murmur3" {
        let hashcode = murmur3::murmur3_32(&mut Cursor::new(text), 0).unwrap();
        return hashcode.to_string();
    } else if algorithm == "xxh32" {
        return xxhash_rust::xxh32::xxh32(text.as_bytes(), 0).to_string();
    } else if algorithm == "xxh64" {
        return xxhash_rust::xxh64::xxh64(text.as_bytes(), 0).to_string();
    }
    format!("{}:{}", algorithm, text)
}

/// HMAC(Hash-based message authentication code) with HmacSHA256 and HmacSHA512
pub fn hmac(algorithm: &str, key: &str, text: &str) -> String {
    if algorithm == "HmacSHA512" {
        let mut mac = HmacSha512::new_from_slice(key.as_bytes()).unwrap();
        mac.update(text.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    } else {
        let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
        mac.update(text.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

pub(crate) fn jwt<'a>(algorithm: &str, key: &str, payload: &StrMap<'a, Str<'a>>) -> String {
    let mut claims: BTreeMap<String, Value> = BTreeMap::new();
    payload.iter(|map| {
        for (key, value) in map {
            let key = key.to_string();
            let value = value.to_string();
            if key == "exp" || key == "nbf" || key == "iat" {
                claims.insert(key, Value::Number(Number::from(value.parse::<u64>().unwrap())));
            } else {
                if let Ok(value) = value.parse::<i64>() {
                    claims.insert(key, Value::Number(Number::from(value)));
                } else if let Ok(value) = value.parse::<f64>() {
                    claims.insert(key, Value::Number(Number::from_f64(value).unwrap()));
                } else {
                    claims.insert(key, Value::String(value));
                }
            }
        }
    });
    let jwt_algorithm = Algorithm::from_str(&algorithm.to_uppercase()).unwrap();
    let encoding_key = match jwt_algorithm {
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
            EncodingKey::from_secret(key.as_ref())
        }
        Algorithm::ES256 | Algorithm::ES384 => {
            EncodingKey::from_ec_pem(key.as_ref()).unwrap()
        }
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => {
            EncodingKey::from_rsa_pem(key.as_ref()).unwrap()
        }
        Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512 => {
            EncodingKey::from_rsa_pem(key.as_ref()).unwrap()
        }
        Algorithm::EdDSA => {
            EncodingKey::from_ed_pem(key.as_ref()).unwrap()
        }
    };
    let header = Header::new(jwt_algorithm);
    jsonwebtoken::encode(&header, &claims, &encoding_key).unwrap()
}

const HMAC_ALGORITHMS: &[Algorithm] = &[Algorithm::HS256, Algorithm::HS384, Algorithm::HS512];
const RSA_ALGORITHMS: &[Algorithm] = &[
    Algorithm::RS256, Algorithm::RS384, Algorithm::RS512,
    Algorithm::PS256, Algorithm::PS384, Algorithm::PS512,
];

/// Build decoding key from PEM text, key type must match the algorithm
fn decoding_key_from_pem(alg: Algorithm, pem: &str) -> Option<DecodingKey> {
    match alg {
        Algorithm::ES256 | Algorithm::ES384 => DecodingKey::from_ec_pem(pem.as_ref()).ok(),
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512
        | Algorithm::PS256 | Algorithm::PS384 | Algorithm::PS512 => DecodingKey::from_rsa_pem(pem.as_ref()).ok(),
        Algorithm::EdDSA => DecodingKey::from_ed_pem(pem.as_ref()).ok(),
        // never use public PEM text as HMAC secret: algorithm confusion attack
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => None,
    }
}

/// Algorithms allowed by JWK: decided by the key type and the optional `alg` of the JWK, not by the token
fn jwk_allowed_algorithms(jwk: &Jwk) -> Vec<Algorithm> {
    let by_key_type: Vec<Algorithm> = match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => RSA_ALGORITHMS.to_vec(),
        AlgorithmParameters::EllipticCurve(params) => match params.curve {
            EllipticCurve::P256 => vec![Algorithm::ES256],
            EllipticCurve::P384 => vec![Algorithm::ES384],
            _ => vec![],
        },
        AlgorithmParameters::OctetKeyPair(_) => vec![Algorithm::EdDSA],
        AlgorithmParameters::OctetKey(_) => HMAC_ALGORITHMS.to_vec(),
    };
    match jwk.common.key_algorithm {
        // KeyAlgorithm and Algorithm share the same names for signature algorithms
        Some(key_alg) => match Algorithm::from_str(&format!("{:?}", key_alg)) {
            Ok(alg) => by_key_type.into_iter().filter(|item| *item == alg).collect(),
            Err(_) => vec![], // encryption only key, e.g. RSA-OAEP
        },
        None => by_key_type,
    }
}

/// JWKS must be fetched over https, http is only allowed for loopback address(local test)
fn is_trusted_jwks_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', '#', '?']).next().unwrap_or("");
        let host = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
        return host == "localhost" || host == "127.0.0.1" || host == "[::1]";
    }
    false
}

pub(crate) fn dejwt<'a>(key: &str, token: &str) -> StrMap<'a, Str<'a>> {
    let map = hashbrown::HashMap::new();
    let header = match decode_header(token) {
        Ok(header) => header,
        Err(_) => return SharedMap::from(map),
    };
    // allowed algorithms are decided by the key, and header.alg must be in the white list
    let decoding_key = if key.starts_with("https://") || key.starts_with("http://") {
        if !is_trusted_jwks_url(key) {
            return SharedMap::from(map);
        }
        let jwk = match extract_jwk(key) {
            Some(jwk) => jwk,
            None => return SharedMap::from(map),
        };
        if !jwk_allowed_algorithms(&jwk).contains(&header.alg) {
            return SharedMap::from(map);
        }
        DecodingKey::from_jwk(&jwk).ok()
    } else if key.trim_start().starts_with("-----BEGIN") {
        decoding_key_from_pem(header.alg, key)
    } else if HMAC_ALGORITHMS.contains(&header.alg) {
        Some(DecodingKey::from_secret(key.as_ref()))
    } else {
        None
    };
    let decoding_key = match decoding_key {
        Some(decoding_key) => decoding_key,
        None => return SharedMap::from(map),
    };
    let mut map = map;
    let validation = Validation::new(header.alg);
    if let Ok(toke_data) = jsonwebtoken::decode::<BTreeMap<String, Value>>(&token, &decoding_key, &validation) {
        for (key, value) in toke_data.claims {
            match value {
                Value::Null => {}
                Value::Bool(bool_value) => {
                    if bool_value {
                        map.insert(Str::from(key), Str::from("1".to_string()));
                    } else {
                        map.insert(Str::from(key), Str::from("0".to_string()));
                    }
                }
                Value::Number(num) => {
                    map.insert(Str::from(key), Str::from(num.to_string()));
                }
                Value::String(text) => {
                    map.insert(Str::from(key), Str::from(text));
                }
                Value::Array(arr) => {
                    map.insert(Str::from(key), Str::from(serde_json::to_string(&arr).unwrap()));
                }
                Value::Object(obj) => {
                    map.insert(Str::from(key), Str::from(serde_json::to_string(&obj).unwrap()));
                }
            }
        }
    }
    SharedMap::from(map)
}

lazy_static! {
    static ref JWK_POOLS: Arc<Mutex<HashMap<String, Jwk>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub fn extract_jwk(full_http_url: &str) -> Option<Jwk> {
    let mut pools = JWK_POOLS.lock().unwrap();
    if let Some(jwk) = pools.get(full_http_url) {
        return Some(jwk.clone());
    }
    let (http_url, kid) = full_http_url.split_once('#')?;
    let jwkset: JwkSet = reqwest::blocking::get(http_url).ok()?.json::<JwkSet>().ok()?;
    let jwk = jwkset.find(kid)?.clone();
    pools.insert(full_http_url.to_string(), jwk.clone());
    Some(jwk)
}

/// Prefix of ciphertext whose key is derived by PBKDF2-HMAC-SHA256 from the password.
/// Ciphertext without this prefix is legacy format: key is the password truncated or zero padded.
const ENCRYPT_V2_PREFIX: &str = "v2:";
const PBKDF2_SALT: &[u8] = b"zawk-encrypt-v2";
const PBKDF2_ITERATIONS: u32 = 100_000;

lazy_static! {
    static ref DERIVED_KEYS: Mutex<HashMap<(String, usize), Vec<u8>>> = Mutex::new(HashMap::new());
}

fn pbkdf2_hmac_sha256(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    let prf = HmacSha256::new_from_slice(password).unwrap();
    for (i, chunk) in out.chunks_mut(32).enumerate() {
        let mut mac = prf.clone();
        mac.update(salt);
        mac.update(&((i as u32) + 1).to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut t = u;
        for _ in 1..iterations {
            let mut mac = prf.clone();
            mac.update(&u);
            u = mac.finalize().into_bytes();
            t.iter_mut().zip(u.iter()).for_each(|(a, b)| *a ^= b);
        }
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
}

/// derive key with PBKDF2, cached by (password, key length) because derivation is slow by design
fn derive_key(key_pass: &str, key_len: usize) -> Vec<u8> {
    let mut keys = DERIVED_KEYS.lock().unwrap();
    keys.entry((key_pass.to_string(), key_len)).or_insert_with(|| {
        let mut key = vec![0u8; key_len];
        pbkdf2_hmac_sha256(key_pass.as_bytes(), PBKDF2_SALT, PBKDF2_ITERATIONS, &mut key);
        key
    }).clone()
}

/// legacy key: password bytes truncated or zero padded to key length
fn legacy_key(key_pass: &str, key_len: usize) -> Vec<u8> {
    let mut key = vec![0u8; key_len];
    let bytes = key_pass.as_bytes();
    let n = bytes.len().min(key_len);
    key[..n].copy_from_slice(&bytes[..n]);
    key
}

/// returns (key length, is GCM)
fn parse_cipher_mode(mode: &str) -> (usize, bool) {
    let key_len = if mode.contains("-256-") { 32 } else { 16 };
    (key_len, mode.ends_with("-gcm"))
}

/// buffer with room for PKCS7 padding (always 1..=16 bytes)
fn cbc_padded_buf(plaintext: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; (plaintext.len() / 16 + 1) * 16];
    buf[..plaintext.len()].copy_from_slice(plaintext);
    buf
}

/// random bytes from the OS CSPRNG, used as IV/nonce
fn random_bytes<const N: usize>() -> Option<[u8; N]> {
    use rand::{rngs::OsRng, TryRngCore};
    let mut buf = [0u8; N];
    OsRng.try_fill_bytes(&mut buf).ok()?;
    Some(buf)
}

fn encrypt_bytes(key: &[u8], gcm: bool, plaintext: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes128Gcm, Aes256Gcm, Nonce};
    let bytes = match (key.len(), gcm) {
        (32, true) => {
            let nonce = Nonce::<U12>::from(random_bytes::<12>()?);
            let ct = Aes256Gcm::new_from_slice(key).ok()?.encrypt(&nonce, plaintext).ok()?;
            [nonce.to_vec(), ct].concat()
        }
        (16, true) => {
            let nonce = Nonce::<U12>::from(random_bytes::<12>()?);
            let ct = Aes128Gcm::new_from_slice(key).ok()?.encrypt(&nonce, plaintext).ok()?;
            [nonce.to_vec(), ct].concat()
        }
        (32, false) => {
            let iv = random_bytes::<16>()?;
            let cipher = Aes256CbcEnc::new_from_slices(key, &iv).ok()?;
            let mut buf = cbc_padded_buf(plaintext);
            let ct = cipher.encrypt_padded::<Pkcs7>(&mut buf, plaintext.len()).ok()?;
            [iv.as_slice(), ct].concat()
        }
        (16, false) => {
            let iv = random_bytes::<16>()?;
            let cipher = Aes128CbcEnc::new_from_slices(key, &iv).ok()?;
            let mut buf = cbc_padded_buf(plaintext);
            let ct = cipher.encrypt_padded::<Pkcs7>(&mut buf, plaintext.len()).ok()?;
            [iv.as_slice(), ct].concat()
        }
        _ => return None,
    };
    Some(bytes)
}

fn decrypt_bytes(key: &[u8], gcm: bool, data: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::{aead::{Aead, KeyInit}, Aes128Gcm, Aes256Gcm, Nonce};
    if gcm {
        // 12 bytes nonce + 16 bytes tag at least
        if data.len() < 12 + 16 {
            return None;
        }
        let (nonce, ciphertext) = data.split_at(12);
        let nonce = Nonce::<U12>::try_from(nonce).ok()?;
        match key.len() {
            32 => Aes256Gcm::new_from_slice(key).ok()?.decrypt(&nonce, ciphertext).ok(),
            16 => Aes128Gcm::new_from_slice(key).ok()?.decrypt(&nonce, ciphertext).ok(),
            _ => None,
        }
    } else {
        // 16 bytes IV + at least one block
        if data.len() < 32 || data.len() % 16 != 0 {
            return None;
        }
        let (iv, ciphertext) = data.split_at(16);
        let mut buf = ciphertext.to_vec();
        match key.len() {
            32 => Aes256CbcDec::new_from_slices(key, iv).ok()?.decrypt_padded::<Pkcs7>(&mut buf).ok().map(|pt| pt.to_vec()),
            16 => Aes128CbcDec::new_from_slices(key, iv).ok()?.decrypt_padded::<Pkcs7>(&mut buf).ok().map(|pt| pt.to_vec()),
            _ => None,
        }
    }
}

/// Encrypt text with a random IV/nonce, output is `v2:` + base64(IV/nonce + ciphertext).
/// Key is derived from key_pass by PBKDF2-HMAC-SHA256. Returns empty string on failure.
pub fn encrypt(mode: &str, plaintext: &str, key_pass: &str) -> String {
    // Using a random IV(Initialization vector) / nonce for GCM has been specified as an official recommendation
    // Initialization Vector for Encryption: https://www.baeldung.com/java-encryption-iv
    let (key_len, gcm) = parse_cipher_mode(mode);
    let key = derive_key(key_pass, key_len);
    match encrypt_bytes(&key, gcm, plaintext.as_bytes()) {
        Some(bytes) => format!("{}{}", ENCRYPT_V2_PREFIX, STANDARD.encode(bytes)),
        None => String::new(),
    }
}

/// Decrypt text produced by `encrypt`, both `v2:` and legacy formats are supported.
/// Returns empty string if the text is malformed, the key is wrong or plaintext is not UTF-8.
pub fn decrypt(mode: &str, encrypted_text: &str, key_pass: &str) -> String {
    let (key_len, gcm) = parse_cipher_mode(mode);
    let (key, encoded) = match encrypted_text.strip_prefix(ENCRYPT_V2_PREFIX) {
        Some(encoded) => (derive_key(key_pass, key_len), encoded),
        None => (legacy_key(key_pass, key_len), encrypted_text),
    };
    STANDARD.decode(encoded.trim()).ok()
        .and_then(|data| decrypt_bytes(&key, gcm, &data))
        .and_then(|pt| String::from_utf8(pt).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::io::BufReader;
    use jsonwebtoken::jwk::JwkSet;
    use crate::runtime::encoding::encode;
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
    fn test_jwt_hs256() {
        let header_payload = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiaWF0IjoxNTE2MjM5MDIyfQ";
        let jwt_token = encode("hex-base64url", &hmac("HmacSha256", "123456", header_payload));
        println!("{}", jwt_token);
    }

    #[test]
    fn test_murmur3() {
        use std::io::Cursor;
        let hash_result = murmur3::murmur3_32(&mut Cursor::new("Hello"), 0);
        println!("{}", hash_result.unwrap());
    }

    #[test]
    fn test_xxh32() {
        let hash_result = xxhash_rust::xxh32::xxh32("hello".as_bytes(), 0).to_string();
        println!("{}", hash_result);
    }

    #[test]
    fn test_adler32() {
        let result = adler::adler32(BufReader::new("demo2".as_bytes())).unwrap();
        println!("{}", result);
    }

    #[test]
    fn test_crc32() {
        let result = crc::Crc::<u32>::new(&crc::CRC_32_CKSUM).checksum(b"123456789");
        println!("{}", result);
    }

    #[test]
    fn test_blake3() {
        println!("{}", digest("blake3", "demo"));
    }

    #[test]
    fn test_jwt() {
        let payload: StrMap<Str> = StrMap::default();
        payload.insert(Str::from("name"), Str::from("John Doe"));
        payload.insert(Str::from("user_uuid"), Str::from("8456ea54-62e8-4a31-9cce-18de7a6a890d"));
        payload.insert(Str::from("user_id"), Str::from("112344"));
        payload.insert(Str::from("rate"), Str::from("11.11"));
        payload.insert(Str::from("exp"), Str::from("1208234234234"));
        let token = jwt("HS256", "123456", &payload);
        println!("HS256: {}", token);
        let pem_text = include_str!("../../tests/jwt-keys/ECDSA-private.pem");
        let token = jwt("ES256", pem_text, &payload);
        println!("ES256: {}", token);
    }

    #[test]
    fn test_dejwt() {
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJleHAiOjEyMDgyMzQyMzQyMzQsIm5hbWUiOiJKb2huIERvZSIsInJhdGUiOjExLjExLCJ1c2VyX2lkIjoxMTIzNDQsInVzZXJfdXVpZCI6Ijg0NTZlYTU0LTYyZTgtNGEzMS05Y2NlLTE4ZGU3YTZhODkwZCJ9.CoS6EPR3qt3-SiMmtU3H3VsndMmO0CWU4s7h9flP184";
        let payload = dejwt("123456", token);
        let value = payload.get(&Str::from("exp"));
        println!("{}", value);
    }

    #[test]
    fn test_dejwt_es256() {
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJleHAiOjEyMDgyMzQyMzQyMzQsIm5hbWUiOiJKb2huIERvZSIsInJhdGUiOjExLjExLCJ1c2VyX2lkIjoxMTIzNDQsInVzZXJfdXVpZCI6Ijg0NTZlYTU0LTYyZTgtNGEzMS05Y2NlLTE4ZGU3YTZhODkwZCJ9.rgIm0ep_VZ1LaoySp0U4dktnMtIhrrXtoo2udzpmYhh_1hQS8-LqgC5j4FRYaXtu8piSZfhCod1aarO_cYDh9Q";
        let pem_text = include_str!("../../tests/jwt-keys/ECDSA-pub.pem");
        let payload = dejwt(pem_text, token);
        assert_eq!(payload.get(&Str::from("exp")).as_str(), "1208234234234");
    }

    #[test]
    fn test_dejwt_alg_confusion() {
        // HS256 token signed with the public PEM text as HMAC secret must be rejected
        let pem_text = include_str!("../../tests/jwt-keys/ECDSA-pub.pem");
        let payload: StrMap<Str> = StrMap::default();
        payload.insert(Str::from("name"), Str::from("attacker"));
        let token = jwt("HS256", pem_text, &payload);
        let claims = dejwt(pem_text, &token);
        assert_eq!(claims.len(), 0);
        // plain secret must not be used for asymmetric algorithms
        let claims = dejwt("123456", "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.e30.sig");
        assert_eq!(claims.len(), 0);
        // invalid token should not panic
        assert_eq!(dejwt("123456", "not-a-token").len(), 0);
    }

    #[test]
    fn test_trusted_jwks_url() {
        assert!(is_trusted_jwks_url("https://example.com/jwks.json#kid"));
        assert!(is_trusted_jwks_url("http://localhost:8000/jwks.json#kid"));
        assert!(!is_trusted_jwks_url("http://example.com/jwks.json#kid"));
        assert!(!is_trusted_jwks_url("http://localhost.evil.com/jwks.json#kid"));
    }

    #[test]
    fn test_jwks_url() {
        // cd tests/jwt-keys && python3 -m http.server
        let full_http_url = "http://localhost:8000/jwks.json#ec-1";
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJleHAiOjEyMDgyMzQyMzQyMzQsIm5hbWUiOiJKb2huIERvZSIsInJhdGUiOjExLjExLCJ1c2VyX2lkIjoxMTIzNDQsInVzZXJfdXVpZCI6Ijg0NTZlYTU0LTYyZTgtNGEzMS05Y2NlLTE4ZGU3YTZhODkwZCJ9.rgIm0ep_VZ1LaoySp0U4dktnMtIhrrXtoo2udzpmYhh_1hQS8-LqgC5j4FRYaXtu8piSZfhCod1aarO_cYDh9Q";
        let payload = dejwt(full_http_url, token);
        let value = payload.get(&Str::from("exp"));
        println!("{}", value);
    }

    #[test]
    fn test_jwks() {
        let keys = include_str!("../../tests/jwt-keys/jwks.json");
        let jwkset: JwkSet = serde_json::from_str(keys).unwrap();
        let jwk = jwkset.find("ec-1").unwrap();
        println!("{:?}", jwk);
        let decoding_key = DecodingKey::from_jwk(jwk).unwrap();
        let token = "eyJ0eXAiOiJKV1QiLCJhbGciOiJFUzI1NiJ9.eyJleHAiOjEyMDgyMzQyMzQyMzQsIm5hbWUiOiJKb2huIERvZSIsInJhdGUiOjExLjExLCJ1c2VyX2lkIjoxMTIzNDQsInVzZXJfdXVpZCI6Ijg0NTZlYTU0LTYyZTgtNGEzMS05Y2NlLTE4ZGU3YTZhODkwZCJ9.rgIm0ep_VZ1LaoySp0U4dktnMtIhrrXtoo2udzpmYhh_1hQS8-LqgC5j4FRYaXtu8piSZfhCod1aarO_cYDh9Q";
        let validation = Validation::new(Algorithm::ES256);
        let token_data = jsonwebtoken::decode::<BTreeMap<String, Value>>(token, &decoding_key, &validation).unwrap();
        println!("{:?}", token_data.claims);
    }

    #[test]
    fn test_aes_cbc() {
        let key_pass = "0123456789abcdef";
        let plaintext = "Hello World";
        let encrypted_text = encrypt("aes-256-cbc", plaintext, key_pass);
        println!("{}", encrypted_text);
        let plaintext2 = decrypt("aes-256-cbc", &encrypted_text, key_pass);
        assert_eq!(plaintext, plaintext2);
    }

    #[test]
    fn test_aes() {
        let key_pass = "0123456789abcdef";
        let plaintext = "Hello World";
        let encrypted_text = encrypt("aes-128-gcm", plaintext, key_pass);
        println!("{}", encrypted_text);
        let plaintext2 = decrypt("aes-128-gcm", &encrypted_text, key_pass);
        assert_eq!(plaintext, plaintext2);
    }

    #[test]
    fn test_aes_256_gcm() {
        let key_pass = "0123456789abcdef";
        let plaintext = "Hello World";
        let encrypted_text = encrypt("aes-256-gcm", plaintext, key_pass);
        println!("{}", encrypted_text);
        let plaintext2 = decrypt("aes-256-gcm", &encrypted_text, key_pass);
        assert_eq!(plaintext, plaintext2);
    }

    #[test]
    fn test_encrypt_long_and_multibyte() {
        let long_text = "x".repeat(5000);
        let key_pass = "密码密码密码密码密码密码密码密码"; // multi-byte chars across byte 16/32
        for mode in ["aes-128-cbc", "aes-256-cbc", "aes-128-gcm", "aes-256-gcm"] {
            let encrypted_text = encrypt(mode, &long_text, key_pass);
            assert!(encrypted_text.starts_with("v2:"));
            assert_eq!(decrypt(mode, &encrypted_text, key_pass), long_text);
            assert_eq!(decrypt(mode, &encrypted_text, "wrong"), "");
            assert_eq!(decrypt(mode, "", key_pass), "");
            assert_eq!(decrypt(mode, "v2:AAAA", key_pass), "");
            assert_eq!(decrypt(mode, "not base64!", key_pass), "");
        }
    }

    #[test]
    fn test_decrypt_legacy() {
        let key_pass = "0123456789abcdef";
        for mode in ["aes-128-cbc", "aes-256-cbc", "aes-128-gcm", "aes-256-gcm"] {
            let (key_len, gcm) = parse_cipher_mode(mode);
            let bytes = encrypt_bytes(&legacy_key(key_pass, key_len), gcm, b"Hello World").unwrap();
            assert_eq!(decrypt(mode, &STANDARD.encode(bytes), key_pass), "Hello World");
        }
    }

    #[test]
    fn test_pbkdf2_vector() {
        // RFC 7914 section 11 PBKDF2-HMAC-SHA256 test vector
        let mut out = [0u8; 64];
        pbkdf2_hmac_sha256(b"passwd", b"salt", 1, &mut out);
        assert_eq!(hex::encode(&out[..16]), "55ac046e56e3089fec1691c22544b605");
    }

    #[test]
    fn test_generate_nonce() {
        let nonce = random_bytes::<12>().unwrap();
        let iv = hex::encode(nonce);
        println!("iv1: {}", iv);
        println!("iv2: {}", hex::encode(get_iv()));
    }

    /// Creates an initial vector (iv). This is also called a nonce
    fn get_iv() -> Vec<u8> {
        let mut iv = vec![];
        for _ in 0..12 {
            let r = rand::random();
            iv.push(r);
        }
        iv
    }
}
