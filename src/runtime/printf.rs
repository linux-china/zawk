//! This module implements printf in awk.
//!
//! Format strings follow C's printf: `%[flags][width][.precision][length]conversion`, with `*`
//! widths and precisions taken from the arguments. Numeric conversions are formatted with the C
//! library's `snprintf` (with a format we build, so it is always well-formed); `%s` and `%c` are
//! formatted here, counting UTF-8 characters for widths and precisions, as gawk does.
use crate::common::Result;
use crate::runtime::{convert, Float, Int, Str};

use std::fmt;
use std::io::Write;
use std::str;

#[derive(Clone, Debug)]
pub(crate) enum FormatArg<'a> {
    S(Str<'a>),
    F(Float),
    I(Int),
    Null,
}

impl<'a> From<Str<'a>> for FormatArg<'a> {
    fn from(s: Str<'a>) -> FormatArg<'a> {
        FormatArg::S(s)
    }
}

impl<'a> From<&'a str> for FormatArg<'a> {
    fn from(s: &'a str) -> FormatArg<'a> {
        FormatArg::S(s.into())
    }
}

impl<'a> From<&'a [u8]> for FormatArg<'a> {
    fn from(bs: &'a [u8]) -> FormatArg<'a> {
        FormatArg::S(bs.into())
    }
}

impl<'a> From<Int> for FormatArg<'a> {
    fn from(i: Int) -> FormatArg<'a> {
        FormatArg::I(i)
    }
}

impl<'a> From<Float> for FormatArg<'a> {
    fn from(f: Float) -> FormatArg<'a> {
        FormatArg::F(f)
    }
}

impl<'a> FormatArg<'a> {
    fn to_float(&self) -> f64 {
        use FormatArg::*;
        match self {
            S(s) => convert::<_, f64>(s),
            F(f) => *f,
            I(i) => convert::<_, f64>(*i),
            Null => 0.0,
        }
    }
    fn to_int(&self) -> i64 {
        use FormatArg::*;
        match self {
            S(s) => convert::<_, i64>(s),
            F(f) => convert::<_, i64>(*f),
            I(i) => *i,
            Null => 0,
        }
    }
    fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        use FormatArg::*;
        let s: Str<'a> = match self {
            S(s) => s.clone(),
            F(f) => convert::<_, Str>(*f),
            I(i) => convert::<_, Str>(*i),
            Null => return f(&[]),
        };
        s.with_bytes(f)
    }
}

/// A parsed conversion specification: `%[flags][width][.precision][length]conversion`.
#[derive(Default)]
struct Spec {
    minus: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: Option<usize>,
    precision: Option<usize>,
    conv: u8,
}

impl Spec {
    /// The (NUL-terminated) C format string for this spec, for a numeric conversion `conv` with
    /// optional length modifier `len` (e.g. "ll").
    fn c_format(&self, len: &[u8], conv: u8) -> CFormat {
        let mut f = CFormat { buf: [0; 64], len: 0 };
        f.push(b"%");
        for (set, c) in [
            (self.minus, b"-"),
            (self.plus, b"+"),
            (self.space, b" "),
            (self.alt, b"#"),
            (self.zero, b"0"),
        ] {
            if set {
                f.push(c);
            }
        }
        if let Some(w) = self.width {
            f.push(itoa::Buffer::new().format(w).as_bytes());
        }
        if let Some(p) = self.precision {
            f.push(b".");
            f.push(itoa::Buffer::new().format(p).as_bytes());
        }
        f.push(len);
        f.push(&[conv]);
        f
    }

    /// Pad `s` (whose width in characters is `chars`) to the field width with spaces.
    fn pad(&self, mut w: impl Write, s: &[u8], chars: usize) -> Result<()> {
        let fill = self.width.unwrap_or(0).saturating_sub(chars);
        if self.minus {
            write_bytes(&mut w, s)?;
            write_repeated(&mut w, b' ', fill)
        } else {
            write_repeated(&mut w, b' ', fill)?;
            write_bytes(&mut w, s)
        }
    }

    /// C's "%d" conversion (with this spec's flags, width and precision) of `i`.
    fn write_int(&self, mut w: impl Write, i: Int) -> Result<()> {
        let mut digits_buf = itoa::Buffer::new();
        let mut digits = digits_buf.format(i.unsigned_abs()).as_bytes();
        if self.precision == Some(0) && i == 0 {
            // An explicit zero precision prints no digits for zero.
            digits = b"";
        }
        let zeros = self.precision.map_or(0, |p| p.saturating_sub(digits.len()));
        let sign: &[u8] = if i < 0 {
            b"-"
        } else if self.plus {
            b"+"
        } else if self.space {
            b" "
        } else {
            b""
        };
        let len = sign.len() + zeros + digits.len();
        let fill = self.width.unwrap_or(0).saturating_sub(len);
        if self.minus {
            write_bytes(&mut w, sign)?;
            write_repeated(&mut w, b'0', zeros)?;
            write_bytes(&mut w, digits)?;
            write_repeated(&mut w, b' ', fill)
        } else if self.zero && self.precision.is_none() {
            write_bytes(&mut w, sign)?;
            write_repeated(&mut w, b'0', zeros + fill)?;
            write_bytes(&mut w, digits)
        } else {
            write_repeated(&mut w, b' ', fill)?;
            write_bytes(&mut w, sign)?;
            write_repeated(&mut w, b'0', zeros)?;
            write_bytes(&mut w, digits)
        }
    }
}

/// A NUL-terminated C format string built by `Spec::c_format`.
struct CFormat {
    buf: [u8; 64],
    len: usize,
}

impl CFormat {
    fn push(&mut self, bs: &[u8]) {
        // Leave room for the NUL terminator; formats are short (widths and precisions are at
        // most 20 digits each).
        let n = bs.len().min(self.buf.len() - 1 - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bs[..n]);
        self.len += n;
    }
    fn as_ptr(&self) -> *const libc::c_char {
        debug_assert_eq!(self.buf[self.len], 0);
        self.buf.as_ptr() as *const libc::c_char
    }
}

/// Write `n` copies of `b`.
fn write_repeated(mut w: impl Write, b: u8, mut n: usize) -> Result<()> {
    let chunk = [b; 64];
    while n > 0 {
        let k = n.min(chunk.len());
        write_bytes(&mut w, &chunk[..k])?;
        n -= k;
    }
    Ok(())
}

/// Run `snprintf` with a format containing exactly one conversion, whose argument is `arg`, and
/// write the result.
fn snprintf_with(w: impl Write, fmt: &CFormat, arg: CArg) -> Result<()> {
    // Safety: `fmt` is built by `Spec::c_format` and has exactly one conversion, whose argument
    // type matches `arg`; `dst` has room for `len` bytes.
    let print = |dst: *mut u8, len: usize| unsafe {
        let dst = dst as *mut libc::c_char;
        match arg {
            CArg::Double(d) => libc::snprintf(dst, len, fmt.as_ptr(), d),
            CArg::ULongLong(u) => libc::snprintf(dst, len, fmt.as_ptr(), u),
        }
    };
    let mut buf = [0u8; 128];
    let n = print(buf.as_mut_ptr(), buf.len());
    if n < 0 {
        return Ok(());
    }
    let n = n as usize;
    if n < buf.len() {
        return write_bytes(w, &buf[..n]);
    }
    let mut big = vec![0u8; n + 1];
    let n = print(big.as_mut_ptr(), big.len()).max(0) as usize;
    write_bytes(w, &big[..n])
}

#[derive(Copy, Clone)]
enum CArg {
    Double(libc::c_double),
    ULongLong(libc::c_ulonglong),
}

/// Number of characters in `bs` (bytes, if it is not valid UTF-8).
fn char_count(bs: &[u8]) -> usize {
    match str::from_utf8(bs) {
        Ok(s) => s.chars().count(),
        Err(_) => bs.len(),
    }
}

/// The first `n` characters of `bs` (bytes, if it is not valid UTF-8).
fn take_chars(bs: &[u8], n: usize) -> &[u8] {
    match str::from_utf8(bs) {
        Ok(s) => match s.char_indices().nth(n) {
            Some((ix, _)) => &bs[..ix],
            None => bs,
        },
        Err(_) => &bs[..n.min(bs.len())],
    }
}

/// Infinities and NaNs are written as in gawk (and padded like strings).
fn special_float(f: f64) -> Option<&'static [u8]> {
    if f.is_nan() {
        Some(if f.is_sign_negative() { b"-nan" } else { b"+nan" })
    } else if f.is_infinite() {
        Some(if f < 0.0 { b"-inf" } else { b"+inf" })
    } else {
        None
    }
}

impl<'a> FormatArg<'a> {
    /// The value of an argument for an integer conversion, which may not fit in an i64.
    fn to_integer(&self) -> std::result::Result<Int, f64> {
        let f = match self {
            FormatArg::I(i) => return Ok(*i),
            FormatArg::Null => return Ok(0),
            _ => self.to_float().trunc(),
        };
        if f.is_finite() && f >= -9.223372036854776e18 && f < 9.223372036854776e18 {
            Ok(f as Int)
        } else {
            Err(f)
        }
    }
}

fn process_spec(w: impl Write, spec: &Spec, arg: &FormatArg) -> Result<()> {
    match spec.conv {
        b'd' | b'i' => match arg.to_integer() {
            Ok(i) => spec.write_int(w, i),
            Err(f) => match special_float(f) {
                Some(s) => spec.pad(w, s, s.len()),
                // Integral values beyond the range of an i64.
                None => {
                    let fmt = Spec { precision: Some(0), ..*spec };
                    snprintf_with(w, &fmt.c_format(b"", b'f'), CArg::Double(f))
                }
            },
        },
        b'o' | b'u' | b'x' | b'X' => {
            // Negative values are written as their two's complement, as in gawk.
            let u = match arg.to_integer() {
                Ok(i) => i as u64,
                Err(f) if f.is_finite() && f > 0.0 => f as u64,
                Err(_) => 0,
            };
            snprintf_with(w, &spec.c_format(b"ll", spec.conv), CArg::ULongLong(u))
        }
        b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a' | b'A' => {
            let f = arg.to_float();
            match special_float(f) {
                Some(s) => spec.pad(w, s, s.len()),
                None => snprintf_with(w, &spec.c_format(b"", spec.conv), CArg::Double(f)),
            }
        }
        b'c' => {
            let mut buf = [0u8; 4];
            let bytes: Vec<u8> = match arg {
                // A string: its first character (a NUL byte for an empty string, as in gawk).
                FormatArg::S(_) => arg.with_bytes(|bs| {
                    if bs.is_empty() {
                        vec![0]
                    } else {
                        take_chars(bs, 1).to_vec()
                    }
                }),
                // A number: the character with that code.
                _ => match char::from_u32(arg.to_int() as u32) {
                    Some(c) => c.encode_utf8(&mut buf).as_bytes().to_vec(),
                    None => vec![arg.to_int() as u8],
                },
            };
            spec.pad(w, &bytes, 1)
        }
        b's' => arg.with_bytes(|bs| {
            let bs = match spec.precision {
                Some(p) => take_chars(bs, p),
                None => bs,
            };
            spec.pad(w, bs, char_count(bs))
        }),
        c => err!("unsupported format specifier: {}", c as char),
    }
}

pub(crate) fn format<'a>(spec: &Str, args: &[FormatArg]) -> Result<Str<'a>> {
    let mut buf = crate::runtime::str_impl::DynamicBuf::default();
    spec.with_bytes(|bs| printf(&mut buf, bs, args))?;
    Ok(buf.into_str())
}

fn wrap_result<T>(r: std::result::Result<T, impl fmt::Display>) -> Result<()> {
    match r {
        Ok(_) => Ok(()),
        Err(e) => err!("formatter: {}", e),
    }
}

fn write_bytes(mut w: impl Write, bs: &[u8]) -> Result<()> {
    wrap_result(w.write_all(bs))
}

/// Parse the conversion specification at `fmt[i..]` (just after a `%`), taking `*` widths and
/// precisions from `next_int`. Returns the spec and the index after it, or `None` if it is not a
/// valid specification (in which case it is written literally, as in awk).
fn parse_spec(
    fmt: &[u8],
    mut i: usize,
    next_int: &mut impl FnMut() -> Int,
) -> Option<(Spec, usize)> {
    let mut spec = Spec::default();
    while let Some(c) = fmt.get(i) {
        match c {
            b'-' => spec.minus = true,
            b'+' => spec.plus = true,
            b' ' => spec.space = true,
            b'#' => spec.alt = true,
            b'0' => spec.zero = true,
            _ => break,
        }
        i += 1;
    }
    let number = |i: &mut usize| -> Option<usize> {
        let start = *i;
        while fmt.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        if *i == start {
            None
        } else {
            str::from_utf8(&fmt[start..*i]).ok()?.parse().ok()
        }
    };
    if fmt.get(i) == Some(&b'*') {
        i += 1;
        let w = next_int();
        if w < 0 {
            spec.minus = true;
        }
        spec.width = Some(w.unsigned_abs() as usize);
    } else {
        spec.width = number(&mut i);
    }
    if fmt.get(i) == Some(&b'.') {
        i += 1;
        if fmt.get(i) == Some(&b'*') {
            i += 1;
            let p = next_int();
            // A negative precision is taken as if it were omitted.
            spec.precision = if p < 0 { None } else { Some(p as usize) };
        } else {
            spec.precision = Some(number(&mut i).unwrap_or(0));
        }
    }
    // Length modifiers are accepted and ignored.
    while matches!(fmt.get(i), Some(b'h' | b'l' | b'L' | b'q' | b'j' | b'z' | b't')) {
        i += 1;
    }
    let conv = *fmt.get(i)?;
    if !matches!(
        conv,
        b'd' | b'i' | b'o' | b'u' | b'x' | b'X' | b'e' | b'E' | b'f' | b'F' | b'g' | b'G' | b'a'
            | b'A' | b'c' | b's' | b'%'
    ) {
        return None;
    }
    spec.conv = conv;
    Some((spec, i + 1))
}

pub(crate) fn printf(mut w: impl Write, fmt: &[u8], args: &[FormatArg]) -> Result<()> {
    // Missing arguments are empty strings (as in onetrue awk).
    let default = FormatArg::S(Default::default());
    let mut arg_ix = 0;
    let mut i = 0;
    let mut raw_start = 0;
    while i < fmt.len() {
        if fmt[i] != b'%' {
            i += 1;
            continue;
        }
        write_bytes(&mut w, &fmt[raw_start..i])?;
        let mut next_int = || {
            let res = args.get(arg_ix).map_or(0, FormatArg::to_int);
            arg_ix += 1;
            res
        };
        match parse_spec(fmt, i + 1, &mut next_int) {
            Some((spec, end)) => {
                if spec.conv == b'%' {
                    // "%%" (with any flags or width) is a literal percent sign.
                    write_bytes(&mut w, b"%")?;
                } else {
                    let arg = args.get(arg_ix).unwrap_or(&default);
                    arg_ix += 1;
                    process_spec(&mut w, &spec, arg)?;
                }
                i = end;
            }
            None => {
                // Not a conversion: write the `%` literally and continue after it.
                write_bytes(&mut w, b"%")?;
                i += 1;
            }
        }
        raw_start = i;
    }
    write_bytes(&mut w, &fmt[raw_start..])
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    macro_rules! sprintf {
        ($fmt:expr $(, $e:expr)*) => {{
            let mut v = Vec::<u8>::new();
            let w = Cursor::new(&mut v);
            printf(w, $fmt, &[$( $e.into() ),*]).expect("printf failure");
            String::from_utf8(v).expect("printf should produce valid utf8")
        }}
    }

    #[test]
    fn basic_printf() {
        use FormatArg::*;
        let mut v = Vec::<u8>::new();
        let w = Cursor::new(&mut v);
        // We don't use the macro here to test the truncation semantics here.
        printf(
            w,
            b"Hi %s, to my %d friends %f percent of the time: %g!",
            &[S("there".into()), F(2.5), I(1), F(1.25369E23)],
        )
        .expect("printf failed");
        let s = str::from_utf8(&v[..]).unwrap();
        assert_eq!(
            s,
            "Hi there, to my 2 friends 1.000000 percent of the time: 1.25369e+23!"
        );

        let s2 = sprintf!(b"%e %d ~~ %s", 12535, 3, "hi");
        assert_eq!(s2.as_str(), "1.253500e+04 3 ~~ hi");
    }

    #[test]
    fn truncation_padding() {
        let s1 = sprintf!(b"%06o |%-10.3s|", 98, "February");
        assert_eq!(s1.as_str(), "000142 |Feb       |");
        let s2 = sprintf!(b"|%-10.");
        assert_eq!(s2.as_str(), "|%-10.");
    }

    #[test]
    fn float_rounding() {
        let s1 = sprintf!(b"%02.2f", 2.375);
        assert_eq!(s1.as_str(), "2.38");
        let s2 = sprintf!(b"%.2f", 2.375);
        assert_eq!(s2.as_str(), "2.38");
    }

    #[test]
    fn c_conversions() {
        // Expected values are gawk's output.
        assert_eq!(
            sprintf!(b"%e %E %.2e %g %G %g %g %#g", 12345.678, 1e-10, 0, 100000, 1e-10, 0.0001, 123456789, 1),
            "1.234568e+04 1.000000E-10 0.00e+00 100000 1E-10 0.0001 1.23457e+08 1.00000"
        );
        assert_eq!(
            sprintf!(b"%i %u %X %#x %#o %+d % d", 3.9, -1, 255, 255, 8, 5, 5),
            "3 18446744073709551615 FF 0xff 010 +5  5"
        );
        assert_eq!(
            sprintf!(b"[%*d][%-*d][%.*f][%*d]", 5, 42, 4, 42, 2, 3.14159, -4, 7),
            "[   42][42  ][3.14][7   ]"
        );
        assert_eq!(sprintf!(b"[%d][%x][%5%][%z]", 9.223372036854775808e18, -1), "[9223372036854775808][ffffffffffffffff][%][%z]");
        assert_eq!(sprintf!(b"[%f][%5.1f]", f64::INFINITY, f64::NAN), "[+inf][ +nan]");
    }

    #[test]
    fn native_int_matches_snprintf() {
        let values = [0, 1, -1, 7, -7, 42, 12345, -12345, i64::MAX, i64::MIN];
        for flags in 0..32u32 {
            for width in [None, Some(0), Some(1), Some(5), Some(12), Some(25)] {
                for precision in [None, Some(0), Some(1), Some(3), Some(8), Some(21)] {
                    let spec = Spec {
                        minus: flags & 1 != 0,
                        plus: flags & 2 != 0,
                        space: flags & 4 != 0,
                        alt: flags & 8 != 0,
                        zero: flags & 16 != 0,
                        width,
                        precision,
                        conv: b'd',
                    };
                    for &i in &values {
                        let mut native = Vec::new();
                        spec.write_int(&mut native, i).unwrap();
                        let fmt = spec.c_format(b"ll", b'd');
                        let mut buf = [0u8; 128];
                        let n = unsafe {
                            libc::snprintf(
                                buf.as_mut_ptr() as *mut libc::c_char,
                                buf.len(),
                                fmt.as_ptr(),
                                i as libc::c_longlong,
                            )
                        } as usize;
                        assert_eq!(
                            String::from_utf8_lossy(&native),
                            String::from_utf8_lossy(&buf[..n]),
                            "{} with {}",
                            i,
                            String::from_utf8_lossy(&fmt.buf[..fmt.len])
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn strings_and_chars() {
        assert_eq!(
            sprintf!(b"[%05s][%.2s][%5.1s][%c][%c][%c][%3c]", "ab", "h\u{e9}llo", "xyz", "hello", 65, 256, "x"),
            "[   ab][h\u{e9}][    x][h][A][\u{100}][  x]"
        );
        assert_eq!(sprintf!(b"%5s|%-5s|", "\u{4f60}\u{597d}", "\u{4f60}\u{597d}"), "   \u{4f60}\u{597d}|\u{4f60}\u{597d}   |");
        assert_eq!(sprintf!(b"[%c]", ""), "[\0]");
    }

    #[test]
    fn literal_percent() {
        assert_eq!(sprintf!(b"100%%").as_str(), "100%");
        assert_eq!(sprintf!(b"%%").as_str(), "%");
        // "%%" must not consume an argument.
        assert_eq!(sprintf!(b"%d%% of %s", 5, "x").as_str(), "5% of x");
        assert_eq!(sprintf!(b"%%%d%%", 7).as_str(), "%7%");
    }
}
