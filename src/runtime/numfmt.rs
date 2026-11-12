//! Converting numbers to strings the way awk does.
//!
//! Integral values are written as integers (with all of their digits). Other values are formatted
//! with CONVFMT, or with OFMT when they are printed with `print`; both default to "%.6g".
//! Infinities and NaNs are written as "+inf", "-inf", "+nan" and "-nan", as in gawk.
//!
//! CONVFMT and OFMT are kept in thread-locals so that the many places converting numbers to
//! strings do not need access to the runtime; `Variables` updates them when they are assigned.
use std::cell::RefCell;
use std::ffi::CString;

use crate::runtime::Float;

/// The argument a validated format takes.
#[derive(Copy, Clone, PartialEq, Debug)]
enum ArgKind {
    Double,
    // An integer conversion (as in gawk, the value is truncated); the format has been rewritten
    // to take a `long long`.
    LongLong,
}

/// A validated printf-style format with exactly one conversion, or `None` for an invalid format,
/// in which case "%.6g" is used.
struct NumFormat {
    fmt: Option<(CString, ArgKind)>,
    // The precision of a plain "%.Ng" format (N ≤ 15, or "%g", precision 6), which is formatted
    // without `snprintf` (see `format_g`).
    fast_g: Option<usize>,
}

impl NumFormat {
    fn new(fmt: &[u8]) -> NumFormat {
        NumFormat {
            fmt: validate_format(fmt).and_then(|(fmt, kind)| Some((CString::new(fmt).ok()?, kind))),
            // "%.16g" and "%.17g" would mostly take the slow path of `g_digits`.
            fast_g: plain_g_precision(fmt).filter(|&p| p <= 15),
        }
    }
    fn default_format() -> NumFormat {
        NumFormat::new(b"%.6g")
    }
}

thread_local! {
    static CONVFMT: RefCell<NumFormat> = RefCell::new(NumFormat::default_format());
    static OFMT: RefCell<NumFormat> = RefCell::new(NumFormat::default_format());
}

/// Set the format used to convert numbers to strings (CONVFMT) on this thread.
pub(crate) fn set_convfmt(fmt: &[u8]) {
    CONVFMT.with(|f| *f.borrow_mut() = NumFormat::new(fmt));
}

/// Set the format used to print numbers (OFMT) on this thread.
pub(crate) fn set_ofmt(fmt: &[u8]) {
    OFMT.with(|f| *f.borrow_mut() = NumFormat::new(fmt));
}

/// Validate `fmt`: it must contain exactly one conversion with optional flags, width and
/// precision, of a floating point value ("%e", "%f", "%g" or "%a", in either case) or an integer
/// ("%d", "%i", "%o", "%u", "%x", "%X"); other text and "%%" are allowed. Only such formats can
/// safely be passed to `snprintf`. Returns the format to pass to `snprintf` (integer conversions
/// take a `long long`) and the kind of its argument.
fn validate_format(fmt: &[u8]) -> Option<(Vec<u8>, ArgKind)> {
    let mut res = Vec::with_capacity(fmt.len() + 2);
    let mut kind = None;
    let mut i = 0;
    while i < fmt.len() {
        match fmt[i] {
            0 => return None,
            b'%' => {
                let start = i;
                i += 1;
                if fmt.get(i) == Some(&b'%') {
                    res.extend_from_slice(b"%%");
                    i += 1;
                    continue;
                }
                while i < fmt.len() && matches!(fmt[i], b'-' | b'+' | b' ' | b'#' | b'0') {
                    i += 1;
                }
                while i < fmt.len() && fmt[i].is_ascii_digit() {
                    i += 1;
                }
                if fmt.get(i) == Some(&b'.') {
                    i += 1;
                    while i < fmt.len() && fmt[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let conv = *fmt.get(i)?;
                let this_kind = match conv {
                    b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => ArgKind::Double,
                    b'd' | b'i' | b'o' | b'u' | b'x' | b'X' => ArgKind::LongLong,
                    _ => return None,
                };
                if kind.replace(this_kind).is_some() {
                    // More than one conversion.
                    return None;
                }
                res.extend_from_slice(&fmt[start..i]);
                if this_kind == ArgKind::LongLong {
                    res.extend_from_slice(b"ll");
                }
                res.push(conv);
                i += 1;
            }
            c => {
                res.push(c);
                i += 1;
            }
        }
    }
    Some((res, kind?))
}

/// The precision of `fmt` if it is exactly "%g" or "%.Ng" (N ≤ 17, "%.g" and "%.0g" meaning 1),
/// with no flags, width or other text.
fn plain_g_precision(fmt: &[u8]) -> Option<usize> {
    let p = match fmt {
        b"%g" => 6,
        [b'%', b'.', digits @ .., b'g']
            if digits.len() <= 2 && digits.iter().all(u8::is_ascii_digit) =>
        {
            digits.iter().fold(0, |n, d| n * 10 + (d - b'0') as usize)
        }
        _ => return None,
    };
    (p <= 17).then_some(p.max(1))
}

/// The significant digits (ASCII, without trailing zeros, in `digits`, whose length is returned)
/// and the decimal exponent of the finite, positive `f` rounded to `p` digits, rounding its exact
/// binary value to nearest, ties to even, as libc does.
fn g_digits(f: Float, p: usize, digits: &mut [u8; 17]) -> (usize, i32) {
    // Fast path: round the shortest round-trip representation (`ryu`) instead. For p ≤ 15 this
    // gives the same digits, unless the representation is a tie at p digits (its digit p + 1 is
    // its last one and a 5), as the true value may be on either side: a p-digit rounding boundary
    // can not lie strictly between the value and its shortest representation, which is within
    // half an ulp (< 1.2e-16 relative, for normal numbers) and is the nearest one of its length.
    if p <= 15 && f >= Float::MIN_POSITIVE {
        let mut rbuf = ryu::Buffer::new();
        let text = rbuf.format_finite(f).as_bytes();
        // `text` is "123.45", "0.00012", "1.5e-7" or "1e30".
        let (mant, e) = match text.iter().position(|&c| c == b'e') {
            Some(i) => (&text[..i], parse_exp(&text[i + 1..])),
            None => (text, 0),
        };
        // The digits without leading zeros, and the number of them (possibly negative, counting
        // leading zeros after the point) before the point.
        let mut all = [0u8; 40];
        let mut na = 0;
        let mut point: Option<isize> = None;
        let mut skipped_after_point = 0isize;
        for &c in mant {
            if c == b'.' {
                point = Some(na as isize);
            } else if na > 0 || c != b'0' {
                all[na] = c;
                na += 1;
            } else if point.is_some() {
                skipped_after_point += 1;
            }
        }
        let int_digits = point.unwrap_or(na as isize) - skipped_after_point;
        while na > 1 && all[na - 1] == b'0' {
            na -= 1;
        }
        let mut exp = (int_digits - 1) as i32 + e;
        if na <= p {
            digits[..na].copy_from_slice(&all[..na]);
            return (na, exp);
        }
        let next = all[p];
        if !(next == b'5' && na == p + 1) {
            let mut nd = p;
            digits[..p].copy_from_slice(&all[..p]);
            if next >= b'5' {
                // Round up, carrying through 9s.
                while nd > 0 && digits[nd - 1] == b'9' {
                    nd -= 1;
                }
                if nd == 0 {
                    digits[0] = b'1';
                    exp += 1;
                    return (1, exp);
                }
                digits[nd - 1] += 1;
            }
            while nd > 1 && digits[nd - 1] == b'0' {
                nd -= 1;
            }
            return (nd, exp);
        }
    }
    g_digits_exact(f, p, digits)
}

/// `g_digits` with Rust's `{:.*e}`, which rounds the exact binary value (it is slower than libc's
/// `snprintf` though).
fn g_digits_exact(f: Float, p: usize, digits: &mut [u8; 17]) -> (usize, i32) {
    use std::io::Write;
    // "d.ddde-X", at most 1 + 1 + 16 + 1 + 4 bytes.
    let mut sci = [0u8; 32];
    let sci_len = {
        let mut w = &mut sci[..];
        write!(w, "{:.*e}", p - 1, f).expect("sci buffer too small");
        32 - w.len()
    };
    let sci = &sci[..sci_len];
    let e_pos = sci.iter().position(|&c| c == b'e').unwrap();
    let exp = parse_exp(&sci[e_pos + 1..]);
    let mut nd = 0;
    for &c in &sci[..e_pos] {
        if c != b'.' {
            digits[nd] = c;
            nd += 1;
        }
    }
    // Drop trailing zeros (there is always a non-zero leading digit).
    while nd > 1 && digits[nd - 1] == b'0' {
        nd -= 1;
    }
    (nd, exp)
}

/// Parse a decimal exponent such as "-7" or "30".
fn parse_exp(bs: &[u8]) -> i32 {
    let (neg, bs) = match bs.split_first() {
        Some((b'-', rest)) => (true, rest),
        _ => (false, bs),
    };
    let v = bs.iter().fold(0i32, |n, d| n * 10 + (d - b'0') as i32);
    if neg { -v } else { v }
}

/// Format the finite, non-zero `f` like C's `printf("%.Pg", f)` into `buf`, returning the length:
/// with the digits of `f` rounded to P significant digits and their decimal exponent X, the fixed
/// notation is used when -4 ≤ X < P and the exponential one otherwise, and trailing zeros (and a
/// trailing ".") are removed.
fn format_g(f: Float, p: usize, buf: &mut [u8; 64]) -> usize {
    debug_assert!(f.is_finite() && f != 0.0 && (1..=17).contains(&p));
    let mut digit_buf = [0u8; 17];
    let (nd, exp) = g_digits(f.abs(), p, &mut digit_buf);
    let digits = &digit_buf[..nd];
    let mut n = 0;
    let mut push = |bs: &[u8]| {
        buf[n..n + bs.len()].copy_from_slice(bs);
        n += bs.len();
    };
    if f < 0.0 {
        push(b"-");
    }
    if exp < -4 || exp >= p as i32 {
        push(&digits[..1]);
        if digits.len() > 1 {
            push(b".");
            push(&digits[1..]);
        }
        let abs = exp.unsigned_abs();
        push(if exp < 0 { b"e-" } else { b"e+" });
        if abs < 10 {
            push(b"0");
        }
        push(itoa::Buffer::new().format(abs).as_bytes());
    } else if exp < 0 {
        push(b"0.");
        for _ in 0..(-exp - 1) {
            push(b"0");
        }
        push(digits);
    } else {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            push(digits);
            for _ in digits.len()..int_len {
                push(b"0");
            }
        } else {
            push(&digits[..int_len]);
            push(b".");
            push(&digits[int_len..]);
        }
    }
    n
}

/// Format `f` with `fmt` (CONVFMT or OFMT) into `buf`, returning the formatted bytes. Uses
/// `buf` unless the output does not fit, in which case it is allocated.
fn format_with<'b>(fmt: &NumFormat, f: Float, buf: &'b mut [u8; 64]) -> std::borrow::Cow<'b, [u8]> {
    let (cfmt, kind) = match &fmt.fmt {
        Some((fmt, kind)) => (fmt, *kind),
        None => return format_with(&NumFormat::default_format(), f, buf),
    };
    if let Some(p) = fmt.fast_g
        && f.is_finite() && f != 0.0 {
            let n = format_g(f, p, buf);
            return std::borrow::Cow::Borrowed(&buf[..n]);
        }
    // Safety: `fmt` is NUL-terminated and contains exactly one conversion, whose argument type
    // is given by `kind`; `dst` has room for `len` bytes.
    let print = |dst: *mut u8, len: usize| unsafe {
        match kind {
            ArgKind::Double => libc::snprintf(
                dst as *mut libc::c_char,
                len,
                cfmt.as_ptr(),
                f as libc::c_double,
            ),
            ArgKind::LongLong => libc::snprintf(
                dst as *mut libc::c_char,
                len,
                cfmt.as_ptr(),
                f as i64 as libc::c_longlong,
            ),
        }
    };
    let n = print(buf.as_mut_ptr(), buf.len());
    if n < 0 {
        return std::borrow::Cow::Borrowed(&[]);
    }
    let n = n as usize;
    if n < buf.len() {
        return std::borrow::Cow::Borrowed(&buf[..n]);
    }
    let mut big = vec![0u8; n + 1];
    let n = print(big.as_mut_ptr(), big.len()).max(0) as usize;
    big.truncate(n);
    std::borrow::Cow::Owned(big)
}

/// Pass the awk string representation of `f` to `k`: integral values and infinities/NaNs are
/// written exactly, other values with OFMT (if `output`, i.e. for print) or CONVFMT.
pub(crate) fn with_number_bytes<R>(f: Float, output: bool, k: impl FnOnce(&[u8]) -> R) -> R {
    if f.is_nan() {
        return k(if f.is_sign_negative() {
            b"-nan"
        } else {
            b"+nan"
        });
    }
    if f.is_infinite() {
        return k(if f < 0.0 { b"-inf" } else { b"+inf" });
    }
    if f == f.trunc() {
        if f.abs() < 1e18 {
            // NB: this also prints -0 as "0".
            return k(itoa::Buffer::new().format(f as i64).as_bytes());
        }
        // Integral but large: all of the digits.
        return k(format!("{:.0}", f).as_bytes());
    }
    let mut buf = [0u8; 64];
    let buf_ref = &mut buf;
    let cell = if output { &OFMT } else { &CONVFMT };
    let bytes = cell.with(move |fmt| format_with(&fmt.borrow(), f, buf_ref));
    k(&bytes)
}

/// Text of a float read from an external source (e.g. a database column): integral values below
/// 1e18 as integers, other finite values in their shortest round-trip form (`0.30000000000000004`,
/// `1e301`, `1e-7`), so no precision is lost and huge values stay short; infinities and NaNs as
/// in awk ("+inf", "-inf", "+nan", "-nan").
pub(crate) fn float_to_text(f: f64) -> String {
    if !f.is_finite() || (f == f.trunc() && f.abs() < 1e18) {
        return with_number_bytes(f, false, |bs| String::from_utf8_lossy(bs).into_owned());
    }
    let mut buf = ryu::Buffer::new();
    let text = buf.format_finite(f);
    text.strip_suffix(".0").unwrap_or(text).to_string()
}

/// `float_to_text` of a single precision float, in the shortest form of the `f32` (`1.1`, not
/// `1.100000023841858`).
pub(crate) fn f32_to_text(f: f32) -> String {
    if !f.is_finite() || (f == f.trunc() && f.abs() < 1e18) {
        return float_to_text(f as f64);
    }
    let mut buf = ryu::Buffer::new();
    let text = buf.format_finite(f);
    text.strip_suffix(".0").unwrap_or(text).to_string()
}

/// Convert a number to a string (e.g. for concatenation or array subscripts), using CONVFMT.
#[cfg(test)]
pub(crate) fn number_to_string(f: Float) -> String {
    with_number_bytes(f, false, |bs| String::from_utf8_lossy(bs).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let valid = |fmt: &[u8]| validate_format(fmt).is_some();
        assert!(valid(b"%.6g"));
        assert!(valid(b"%.2f"));
        assert!(valid(b"x=%-10.3e%%"));
        assert!(valid(b"%d"));
        assert!(valid(b"%05x"));
        assert_eq!(
            validate_format(b"%5d"),
            Some((b"%5lld".to_vec(), ArgKind::LongLong))
        );
        for bad in [
            &b"%s"[..],
            b"%Lf",
            b"%ld",
            b"%*g",
            b"%g %g",
            b"%d%f",
            b"abc",
            b"%",
            b"%.2f\0",
        ] {
            assert!(!valid(bad), "{:?}", bad);
        }
    }

    #[test]
    fn numbers() {
        assert_eq!(number_to_string(0.1 + 0.2), "0.3");
        assert_eq!(number_to_string(1.0 / 3.0), "0.333333");
        assert_eq!(number_to_string(1234567.5), "1.23457e+06");
        assert_eq!(number_to_string(1e17), "100000000000000000");
        assert_eq!(number_to_string(2f64.powi(63)), "9223372036854775808");
        assert_eq!(number_to_string(1e30), "1000000000000000019884624838656");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(-2.5), "-2.5");
        assert_eq!(number_to_string(f64::INFINITY), "+inf");
        assert_eq!(number_to_string(f64::NEG_INFINITY), "-inf");
        assert_eq!(number_to_string(f64::NAN), "+nan");
        set_convfmt(b"%.2f");
        assert_eq!(number_to_string(1.23456), "1.23");
        assert_eq!(number_to_string(3.0), "3");
        set_convfmt(b"%d");
        assert_eq!(number_to_string(3.7), "3");
        set_convfmt(b"%s");
        assert_eq!(number_to_string(1.23456), "1.23456");
        set_convfmt(b"%.6g");
    }

    /// `printf(fmt, f)` with libc.
    fn libc_format(fmt: &str, f: f64) -> String {
        let fmt = CString::new(fmt).unwrap();
        let mut buf = [0u8; 128];
        let n = unsafe {
            libc::snprintf(
                buf.as_mut_ptr() as *mut libc::c_char,
                buf.len(),
                fmt.as_ptr(),
                f,
            )
        };
        String::from_utf8(buf[..n as usize].to_vec()).unwrap()
    }

    fn fast_format(p: usize, f: f64) -> String {
        let mut buf = [0u8; 64];
        let n = format_g(f, p, &mut buf);
        String::from_utf8(buf[..n].to_vec()).unwrap()
    }

    #[test]
    fn g_precisions() {
        assert_eq!(plain_g_precision(b"%.6g"), Some(6));
        assert_eq!(plain_g_precision(b"%g"), Some(6));
        assert_eq!(plain_g_precision(b"%.0g"), Some(1));
        assert_eq!(plain_g_precision(b"%.g"), Some(1));
        assert_eq!(plain_g_precision(b"%.17g"), Some(17));
        for other in [
            &b"%.18g"[..],
            b"%.100g",
            b"%10.6g",
            b"%#.6g",
            b"%.6G",
            b"x%.6g",
            b"%.6f",
        ] {
            assert_eq!(plain_g_precision(other), None, "{:?}", other);
        }
    }

    /// Only non-integral values are compared: integral ones are never formatted with `format_g`
    /// (see `with_number_bytes`), and macOS libc keeps a trailing zero when a tie of an integral
    /// value rounds down in "%g" (`printf("%.7g", 95967105.0)` gives "9.596710e+07").
    #[test]
    fn fast_g_matches_libc() {
        let check = |f: f64| {
            if f.fract() == 0.0 {
                return;
            }
            for p in 1..=17 {
                assert_eq!(
                    fast_format(p, f),
                    libc_format(&format!("%.{}g", p), f),
                    "%.{}g of {:e} ({:#x})",
                    p,
                    f,
                    f.to_bits()
                );
            }
        };
        for f in [
            0.5,
            0.125,
            0.375,
            2.5,
            1.5,
            1e-5,
            1.0000005,
            0.000123456789,
            123456.5,
            999999.5,
            9.9999995,
            0.00009999995,
            1e-300,
            5e-324,
            f64::MIN_POSITIVE,
            10.5,
            100.5,
            0.0625,
            0.1,
            0.2,
            0.3,
            1.0 / 3.0,
            2.0 / 3.0,
            1234567.5,
            1e21 / 7.0,
            12345678901234.5,
        ] {
            check(f);
            check(-f);
        }
        // Pseudo-random values: random bit patterns, and decimals with a few digits (whose
        // rounding is close to a tie).
        let mut x: u64 = 0x9E3779B97F4A7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..20_000 {
            let f = f64::from_bits(next());
            if f.is_finite() {
                check(f);
            }
            // Random bit patterns are mostly huge or tiny; also take ones with exponents near 0.
            let f = f64::from_bits((next() & 0x800F_FFFF_FFFF_FFFF) | ((1013 + next() % 40) << 52));
            check(f);
            let r = next();
            let mant = (r >> 20) % 100_000_000;
            let scale = 10f64.powi((r % 40) as i32 - 20);
            check(mant as f64 * scale);
            check((r % 1_000_000) as f64 / 1000.0 * 1.5);
            check((r % 1_000_000) as f64 / 1024.0);
        }
    }

    #[test]
    fn float_texts() {
        assert_eq!(float_to_text(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(float_to_text(1e301), "1e301");
        assert_eq!(float_to_text(1e-7), "1e-7");
        assert_eq!(float_to_text(42.0), "42");
        assert_eq!(float_to_text(-0.0), "0");
        assert_eq!(float_to_text(1e17), "100000000000000000");
        assert_eq!(float_to_text(1e18), "1e18");
        assert_eq!(float_to_text(-2.5), "-2.5");
        assert_eq!(float_to_text(f64::INFINITY), "+inf");
        assert_eq!(float_to_text(f64::NAN), "+nan");
        assert_eq!(f32_to_text(1.1), "1.1");
        assert_eq!(f32_to_text(3.0), "3");
        assert_eq!(f32_to_text(1e30), "1e30");
    }
}
