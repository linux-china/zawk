//! UTF-8 validation helpers for the input splitters.
//!
//! Backed by `simdutf8`, which selects AVX2 / SSE4.2 / NEON / WASM SIMD
//! implementations (at runtime where needed) and falls back to scalar code.

/// Validates `bs`, which may be a prefix of a longer stream: a multi-byte
/// character cut off at the end is not an error. Returns the length of the
/// valid prefix, or `None` if `bs` contains invalid UTF-8.
pub(crate) fn validate_utf8_clipped(bs: &[u8]) -> Option<usize> {
    // Fast path: the common case is a fully valid buffer.
    if simdutf8::basic::from_utf8(bs).is_ok() {
        return Some(bs.len());
    }
    match simdutf8::compat::from_utf8(bs) {
        Ok(_) => Some(bs.len()),
        // `error_len() == None` means the input ended in the middle of a
        // (so far valid) multi-byte sequence.
        Err(e) if e.error_len().is_none() => Some(e.valid_up_to()),
        Err(_) => None,
    }
}

/// Returns whether `bs` is entirely valid UTF-8.
pub(crate) fn is_utf8(bs: &[u8]) -> bool {
    simdutf8::basic::from_utf8(bs).is_ok()
}

#[cfg(test)]
fn parse_utf8_clipped(bs: &[u8]) -> Option<&str> {
    validate_utf8_clipped(bs).map(|off| unsafe { std::str::from_utf8_unchecked(&bs[..off]) })
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    fn parse_utf8_std(bs: &[u8]) -> Option<&str> {
        match std::str::from_utf8(bs) {
            Ok(s) => Some(s),
            Err(e) if e.error_len().is_none() => {
                Some(unsafe { std::str::from_utf8_unchecked(&bs[..e.valid_up_to()]) })
            }
            Err(_) => None,
        }
    }
    const LEN: usize = 50_000;

    static ASCII: LazyLock<String> = LazyLock::new(|| String::from_utf8(bytes(LEN, 0.0)).unwrap());
    static UTF8: LazyLock<String> = LazyLock::new(|| String::from_utf8(bytes(LEN, 1.0)).unwrap());

    #[test]
    fn test_partial() {
        let bs: Vec<_> = UTF8.as_bytes().to_vec();
        let l = bs.len();
        let full = super::parse_utf8_clipped(&bs[..]).expect("full");
        assert_eq!(UTF8.as_str(), full);
        let partial = super::parse_utf8_clipped(&bs[..l - 1]).expect("partial (1)");
        let partial_fallback = parse_utf8_std(&bs[..l - 1]).expect("partial (2)");
        assert_eq!(partial, partial_fallback);
    }

    #[test]
    fn clipped_edge_cases() {
        use super::validate_utf8_clipped as v;
        assert_eq!(v(b""), Some(0));
        assert_eq!(v(b"abc"), Some(3));
        // "中" = E4 B8 AD, clipped after 1 and 2 bytes
        assert_eq!(v(b"ab\xE4"), Some(2));
        assert_eq!(v(b"ab\xE4\xB8"), Some(2));
        assert_eq!(v(b"ab\xE4\xB8\xAD"), Some(5));
        // 4-byte char clipped after 3 bytes
        assert_eq!(v(b"a\xF0\x9F\x98"), Some(1));
        // invalid bytes, even near the end, are errors rather than clipping
        assert_eq!(v(b"ab\xFF"), None);
        assert_eq!(v(b"ab\xE4a"), None);
        assert_eq!(v(b"\xFFabcdef"), None);
    }

    #[test]
    fn utf8_valid() {
        assert!(std::str::from_utf8(UTF8.as_bytes()).is_ok());
        assert!(super::is_utf8(UTF8.as_bytes()));
        // Corrupt it some
        let mut bs: Vec<_> = UTF8.as_bytes().to_vec();
        let l = bs.len();
        assert!(std::str::from_utf8(&bs[..l - 1]).is_err());
        assert!(!super::is_utf8(&bs[..l - 1]));
        bs[l / 2] = 255;
        bs[l / 3] = 255;
        assert!(!super::is_utf8(&bs[..]));
        assert!(std::str::from_utf8(&bs[..]).is_err());
    }
    #[test]
    fn ascii_valid() {
        assert!(std::str::from_utf8(ASCII.as_bytes()).is_ok());
        assert!(super::is_utf8(ASCII.as_bytes()));
    }

    fn bytes(n: usize, utf8_pct: f64) -> Vec<u8> {
        let mut res = Vec::with_capacity(n);
        use rand::distr::{Distribution, Uniform};
        let ascii = Uniform::new_inclusive(0u8, 127u8).unwrap();
        let between = Uniform::new_inclusive(0.0, 1.0).unwrap();
        let mut rng = rand::rng();
        for _ in 0..n {
            if between.sample(&mut rng) < utf8_pct {
                let c = rand::random::<char>();
                let ix = res.len();
                res.resize(ix + c.len_utf8(), 0);
                c.encode_utf8(&mut res[ix..]);
            } else {
                res.push(ascii.sample(&mut rng))
            }
        }
        res
    }
}

#[cfg(all(feature = "unstable", test))]
mod bench {
    extern crate test;
    use std::sync::LazyLock;
    use test::{Bencher, black_box};

    const LEN: usize = 50_000;

    static ASCII: LazyLock<String> = LazyLock::new(|| String::from_utf8(bytes(LEN, 0.0)).unwrap());
    static UTF8: LazyLock<String> = LazyLock::new(|| String::from_utf8(bytes(LEN, 1.0)).unwrap());

    #[bench]
    fn parse_ascii_stdlib(b: &mut Bencher) {
        let bs = ASCII.as_bytes();
        b.iter(|| {
            black_box(std::str::from_utf8(bs).is_ok());
        })
    }

    #[bench]
    fn parse_ascii_simd(b: &mut Bencher) {
        let bs = ASCII.as_bytes();
        b.iter(|| {
            black_box(super::is_utf8(bs));
        })
    }

    #[bench]
    fn parse_100_utf8_simd(b: &mut Bencher) {
        let bs = bytes(LEN, 1.0);
        b.iter(|| {
            black_box(super::is_utf8(&bs[..]));
        })
    }

    #[bench]
    fn parse_100_utf8_stdlib(b: &mut Bencher) {
        let bs = bytes(LEN, 1.0);
        b.iter(|| {
            black_box(std::str::from_utf8(&bs[..]).is_ok());
        })
    }

    #[bench]
    fn parse_50_utf8_simd(b: &mut Bencher) {
        let bs = bytes(LEN, 0.5);
        b.iter(|| {
            black_box(super::is_utf8(&bs[..]));
        })
    }

    #[bench]
    fn parse_50_utf8_stdlib(b: &mut Bencher) {
        let bs = bytes(LEN, 0.5);
        b.iter(|| {
            black_box(std::str::from_utf8(&bs[..]).is_ok());
        })
    }

    #[bench]
    fn parse_10_utf8_simd(b: &mut Bencher) {
        let bs = bytes(LEN, 0.1);
        b.iter(|| {
            black_box(super::is_utf8(&bs[..]));
        })
    }

    #[bench]
    fn parse_10_utf8_stdlib(b: &mut Bencher) {
        let bs = bytes(LEN, 0.1);
        b.iter(|| {
            black_box(std::str::from_utf8(&bs[..]).is_ok());
        })
    }

    #[bench]
    fn parse_1_utf8_simd(b: &mut Bencher) {
        let bs = bytes(LEN, 0.01);
        b.iter(|| {
            black_box(super::is_utf8(&bs[..]));
        })
    }

    #[bench]
    fn parse_1_utf8_stdlib(b: &mut Bencher) {
        let bs = bytes(LEN, 0.01);
        b.iter(|| {
            black_box(std::str::from_utf8(&bs[..]).is_ok());
        })
    }

    fn bytes(n: usize, utf8_pct: f64) -> Vec<u8> {
        let mut res = Vec::with_capacity(n);
        use rand::distr::{Distribution, Uniform};
        let ascii = Uniform::new_inclusive(0u8, 127u8).unwrap();
        let between = Uniform::new_inclusive(0.0, 1.0).unwrap();
        let mut rng = rand::rng();
        for _ in 0..n {
            if between.sample(&mut rng) < utf8_pct {
                let c = rand::random::<char>();
                let ix = res.len();
                res.resize(res.len() + c.len_utf8(), 0);
                c.encode_utf8(&mut res[ix..]);
            } else {
                res.push(ascii.sample(&mut rng))
            }
        }
        res
    }
}
