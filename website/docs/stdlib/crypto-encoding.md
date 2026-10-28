---
title: Crypto, Encoding & IDs
sidebar_label: Crypto & Encoding
---

# Crypto, Encoding & IDs

## Digest and HMAC

```awk
digest("sha256", $0)        # hash() is an alias
hmac("HmacSHA256", "your-secret-key", $0)
```

Digest algorithms: `md5`, `sha256`, `sha384`, `sha512`, `bcrypt`, `murmur3`, `xxh32`, `xxh64`, `blake3`,
`crc32`, `adler32`. HMAC: `HmacSHA256`, `HmacSHA384`, `HmacSHA512`. Names are case-insensitive.

## Encryption

```awk
secret = encrypt("aes-256-gcm", "Secret Text", "your_pass_key")
plain  = decrypt("aes-256-gcm", secret, "your_pass_key")
```

- Modes: `aes-128-gcm`, `aes-256-gcm` (recommended, authenticated), `aes-128-cbc`, `aes-256-cbc`
  (encrypt-then-MAC).
- The AES key is derived from the password with PBKDF2-HMAC-SHA256 (100,000 iterations) and a random salt.
- A tampered ciphertext or a wrong key fails to decrypt: a warning, an empty result and `ERRNO`.

## JWT

```awk
payload["sub"] = "1234"; payload["exp"] = systime() + 3600
token  = jwt("HS256", "your-secret-key", payload)
claims = dejwt("your-secret-key", token)

token  = jwt("RS256", private_key_pem, payload)        # RS*, ES*, EdDSA
claims = dejwt(public_key_pem, token)
claims = dejwt("https://example.com/jwks.json#kid", token)   # JWKS
```

The allowed algorithms are decided by the key, not by the `alg` of the token header. `exp` and `nbf` are
checked when present. Verify a token in a pipeline:

```shell
zawk '{ c = try dejwt(ENVIRON["JWT_SECRET"], $1) else NONE; print length(c) ? c["sub"] : "invalid" }' tokens.txt
```

## Encoding

`encode(format, s)` and `decode(format, s)`:

| Format | Notes |
|--------|-------|
| `hex`, `base32`, `base58`, `base62`, `base64`, `base64url` | |
| `url` | percent-encoding |
| `zlib2base64url` | zlib then base64url, for [PlantUML](https://plantuml.com/) and [Kroki](https://kroki.io/) |
| `hex-base64`, `hex-base64url`, `base64-hex`, `base64url-hex` | conversions |

```shell
zawk '{ print encode("base64", $0) }' data.txt
zawk 'BEGIN { print "https://kroki.io/plantuml/svg/" encode("zlib2base64url", read_all("diagram.puml")) }'
```

## ID generators

```awk
uuid()          # v4
uuid("v7")      # time-ordered
ulid()          # 01ARZ3NDEKTSV4RRFFQ69G5FAV
tsid()
snowflake(1)    # machine id 0..1023
```

## Fake data

```awk
fake("name"); fake("email"); fake("name", "cn")
```

Data kinds: `name`, `phone`, `cell`, `email`, `wechat`, `ip`, `creditcard`, `zipcode`, `plate`, `postcode`, `id`;
locales `EN` (default) and `CN`.

```shell
zawk -o csv 'BEGIN { print "id", "name", "email"; for (i in 1..=100) print uuid("v7"), fake("name"), fake("email") }' > users.csv
```
