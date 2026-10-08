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
}

impl NumFormat {
    fn new(fmt: &[u8]) -> NumFormat {
        NumFormat {
            fmt: validate_format(fmt)
                .and_then(|(fmt, kind)| Some((CString::new(fmt).ok()?, kind))),
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

/// Format `f` with `fmt` (CONVFMT or OFMT) into `buf`, returning the formatted bytes. Uses
/// `buf` unless the output does not fit, in which case it is allocated.
fn format_with<'b>(fmt: &NumFormat, f: Float, buf: &'b mut [u8; 64]) -> std::borrow::Cow<'b, [u8]> {
    let (fmt, kind) = match &fmt.fmt {
        Some((fmt, kind)) => (fmt, *kind),
        None => return format_with(&NumFormat::default_format(), f, buf),
    };
    // Safety: `fmt` is NUL-terminated and contains exactly one conversion, whose argument type
    // is given by `kind`; `dst` has room for `len` bytes.
    let print = |dst: *mut u8, len: usize| unsafe {
        match kind {
            ArgKind::Double => {
                libc::snprintf(dst as *mut libc::c_char, len, fmt.as_ptr(), f as libc::c_double)
            }
            ArgKind::LongLong => libc::snprintf(
                dst as *mut libc::c_char,
                len,
                fmt.as_ptr(),
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
        return k(if f.is_sign_negative() { b"-nan" } else { b"+nan" });
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
        assert_eq!(validate_format(b"%5d"), Some((b"%5lld".to_vec(), ArgKind::LongLong)));
        for bad in [&b"%s"[..], b"%Lf", b"%ld", b"%*g", b"%g %g", b"%d%f", b"abc", b"%", b"%.2f\0"] {
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
        assert_eq!(number_to_string(3.14159), "3.14");
        assert_eq!(number_to_string(3.0), "3");
        set_convfmt(b"%d");
        assert_eq!(number_to_string(3.7), "3");
        set_convfmt(b"%s");
        assert_eq!(number_to_string(3.14159), "3.14159");
        set_convfmt(b"%.6g");
    }
}
