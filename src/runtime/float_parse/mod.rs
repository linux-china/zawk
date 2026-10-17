//! Fast float parser based on github.com/lemire/fast_double_parser, but adopted to support AWK
//! semantics (no failures, just 0s and stopping early). Mistakes are surely my own.

/// The value of `hex(s)`: leading blanks, an optional sign and an optional `0x`/`0X` prefix, then
/// the hexadecimal digits (as in [`str_to_num`], trailing text is ignored; no digits is 0). The
/// digits are read as a 64-bit unsigned integer, wrapping around on overflow, and returned with
/// the same bits as an i64, so a 64-bit hash or id such as `hex("ffffffffffffffff")` is -1.
pub fn hextoi(bs: &[u8]) -> i64 {
    let is_blank = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c');
    let start = bs.iter().position(|b| !is_blank(b)).unwrap_or(bs.len());
    let (neg, mut digits) = match &bs[start..] {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        body => (false, body),
    };
    if let [b'0', b'x' | b'X', rest @ ..] = digits {
        digits = rest;
    }
    let i = digits
        .iter()
        .map_while(|b| (*b as char).to_digit(16))
        .fold(0u64, |i, d| i.wrapping_mul(16).wrapping_add(d as u64)) as i64;
    if neg {
        i.wrapping_neg()
    } else {
        i
    }
}

/// The value of an integer literal of the program: an integer if it is exact as a double (at most
/// 2^53 in magnitude), a float otherwise, as awk numbers are doubles. This also keeps arithmetic
/// on such literals from overflowing 64-bit integers (`9223372036854775807 + 1`).
pub fn exact_int_literal(lit: Result<i64, f64>) -> Result<i64, f64> {
    match lit {
        Ok(i) if i.unsigned_abs() > 1 << 53 => Err(i as f64),
        lit => lit,
    }
}

/// Parse a decimal integer literal (with an optional sign). Values outside the range of i64 are
/// returned as the nearest float (`Err`), as awk numbers are doubles.
pub fn parse_int_literal(bs: &[u8]) -> Result<i64, f64> {
    std::str::from_utf8(bs)
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or_else(|| strtod(bs))
}

/// Parse a hexadecimal integer literal (`0x...`, with an optional sign). Values outside the range
/// of i64 are returned as the nearest float (`Err`).
pub fn parse_hex_literal(mut bs: &[u8]) -> Result<i64, f64> {
    let neg = bs.first() == Some(&b'-');
    if neg || bs.first() == Some(&b'+') {
        bs = &bs[1..];
    }
    if bs.len() >= 2 && bs[0] == b'0' && (bs[1] == b'x' || bs[1] == b'X') {
        bs = &bs[2..];
    }
    // Accumulate exactly in a u128 while possible, then (for absurdly long literals) in a float.
    let mut exact: Option<u128> = Some(0);
    let mut approx = 0f64;
    for b in bs.iter().cloned() {
        let digit = match b {
            b'A'..=b'F' => b - b'A' + 10,
            b'a'..=b'f' => b - b'a' + 10,
            b'0'..=b'9' => b - b'0',
            _ => break,
        };
        exact = exact
            .and_then(|i| i.checked_mul(16))
            .and_then(|i| i.checked_add(digit as u128));
        approx = approx * 16.0 + digit as f64;
    }
    let exact = exact.map(|i| if neg { -(i as i128) } else { i as i128 });
    match exact.map(i64::try_from) {
        Some(Ok(i)) => Ok(i),
        Some(Err(_)) => Err(exact.unwrap() as f64),
        None => Err(if neg { -approx } else { approx }),
    }
}

/// The value of the hexadecimal digits at the start of `bs` (the longest run of them), and how
/// many there are. Values beyond the range of a u64 are the nearest float.
pub fn hex_digits(bs: &[u8]) -> (f64, usize) {
    let mut exact = 0u64;
    let mut approx: Option<f64> = None;
    let mut n = 0;
    for d in bs.iter().map_while(|b| (*b as char).to_digit(16)) {
        n += 1;
        approx = match approx {
            Some(f) => Some(f * 16.0 + d as f64),
            None => match exact.checked_mul(16).and_then(|i| i.checked_add(d as u64)) {
                Some(i) => {
                    exact = i;
                    None
                }
                None => Some(exact as f64 * 16.0 + d as f64),
            },
        };
    }
    (approx.unwrap_or(exact as f64), n)
}

/// The numeric value of a string, the one conversion used everywhere a string becomes a number
/// (arithmetic, `$1 == 16`, printf, sorting): leading blanks and an optional sign, then a `0x`
/// or `0X` prefixed hexadecimal number, or else a decimal one (see [`strtod`]). Trailing text is
/// ignored, and a string with no number at its start is 0. It never allocates.
pub fn str_to_num(bs: &[u8]) -> f64 {
    let is_blank = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c');
    let start = bs.iter().position(|b| !is_blank(b)).unwrap_or(bs.len());
    let body = &bs[start..];
    let (neg, digits) = match body {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        _ => (false, body),
    };
    if let [b'0', b'x' | b'X', rest @ ..] = digits {
        let (n, len) = hex_digits(rest);
        if len > 0 {
            return if neg { -n } else { n };
        }
    }
    strtod(body)
}

/// [`str_to_num`] truncated toward zero, saturating at the bounds of i64 (NaN is 0).
pub fn str_to_int(bs: &[u8]) -> i64 {
    str_to_num(bs) as i64
}

/// Parse a floating-poing number from `bs`, returning 0 if one isn't there.
///
/// Like gawk, words such as "nan", "info" or "Infinity" are not numbers: the special values are
/// only recognized with an explicit sign and nothing but blanks after them ("+inf", "-nan").
pub fn strtod(bs: &[u8]) -> f64 {
    let is_blank = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0b' | b'\x0c');
    let start = bs.iter().position(|b| !is_blank(b)).unwrap_or(bs.len());
    let bs = &bs[start..];
    let signed = matches!(bs.first(), Some(b'+') | Some(b'-'));
    let body = if signed { &bs[1..] } else { bs };
    match body.first() {
        Some(b) if b.is_ascii_digit() || *b == b'.' => {}
        Some(_) if signed => {
            let end = body.iter().rposition(|b| !is_blank(b)).map_or(0, |i| i + 1);
            let word = &body[..end];
            let neg = bs[0] == b'-';
            if word.eq_ignore_ascii_case(b"inf") {
                return if neg { f64::NEG_INFINITY } else { f64::INFINITY };
            }
            if word.eq_ignore_ascii_case(b"nan") {
                return if neg { -f64::NAN } else { f64::NAN };
            }
            return 0.0f64;
        }
        _ => return 0.0f64,
    }
    if let Ok((f, _)) = fast_float::parse_partial(bs) {
        f
    } else {
        0.0f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_behavior() {
        assert_eq!(strtod(b"1.234"), 1.234);
        assert_eq!(strtod(b"1.234hello"), 1.234);
        assert_eq!(strtod(b"1.234E70hello"), 1.234E70);
        assert_eq!(strtod(b"752834029324532"), 752834029324532.0);
        assert_eq!(strtod(b"-3.463682231963e-01"), -3.463682231963e-01);
        assert_eq!(strtod(b""), 0.0);
        let imax = format!("{}", i64::MAX);
        let imin = format!("{}", i64::MIN);
        assert_eq!(strtod(imax.as_bytes()), i64::MAX as f64);
        assert_eq!(strtod(imin.as_bytes()), i64::MIN as f64);
    }

    #[test]
    fn hextoi_values() {
        for (s, v) in [
            ("0xff", 255),
            ("ff", 255),
            (" ff", 255),
            ("+ff", 255),
            ("-0x10", -16),
            ("0x1fzz", 31),
            ("", 0),
            ("-", 0),
            ("0x", 0),
            ("zz", 0),
            ("7fffffffffffffff", i64::MAX),
            ("ffffffffffffffff", -1),
            ("1ffffffffffffffff", -1),
        ] {
            assert_eq!(hextoi(s.as_bytes()), v, "hextoi({:?})", s);
        }
    }

    #[test]
    fn str_to_num_values() {
        for (s, v) in [
            ("0x10", 16.0),
            ("0X1f", 31.0),
            ("-0x10", -16.0),
            (" +0x10 ", 16.0),
            ("0x1fz", 31.0),
            ("0x", 0.0),
            ("0xg", 0.0),
            ("0x-5", 0.0),
            ("0x+5", 0.0),
            ("0x8000000000000000", 9223372036854775808.0),
            ("0x10000000000000000", 18446744073709551616.0),
            ("0b101", 0.0),
            ("0o17", 0.0),
            ("017", 17.0),
            ("  -1.5abc", -1.5),
            ("1e3", 1000.0),
            ("", 0.0),
            ("abc", 0.0),
        ] {
            assert_eq!(str_to_num(s.as_bytes()), v, "{s:?}");
        }
        assert_eq!(str_to_num(b"-inf"), f64::NEG_INFINITY);
        assert_eq!(str_to_int(b"0x10"), 16);
        assert_eq!(str_to_int(b" 12abc"), 12);
        assert_eq!(str_to_int(b"-7.9"), -7);
        assert_eq!(str_to_int(b"1e3"), 1000);
        assert_eq!(str_to_int(b"1e30"), i64::MAX);
    }

    #[test]
    fn special_values() {
        for s in [
            "nan", "NaN", "inf", "Nancy", "information", "Infinity", "+infinity", "-infx",
            "-inf5", "+", "-", "  info", "e5",
        ] {
            assert_eq!(strtod(s.as_bytes()), 0.0, "{s:?}");
        }
        assert_eq!(strtod(b"+inf"), f64::INFINITY);
        assert_eq!(strtod(b" -INF \n"), f64::NEG_INFINITY);
        assert!(strtod(b"+nan").is_nan());
        assert!(strtod(b"-NaN ").is_nan());
        assert_eq!(strtod(b"  -1.5x"), -1.5);
        assert_eq!(strtod(b".5"), 0.5);
        assert_eq!(strtod(b"+.5e1"), 5.0);
    }

    #[test]
    fn int_literals() {
        assert_eq!(parse_int_literal(b"123"), Ok(123));
        assert_eq!(parse_int_literal(b"-9223372036854775808"), Ok(i64::MIN));
        assert_eq!(parse_int_literal(b"9223372036854775807"), Ok(i64::MAX));
        assert_eq!(parse_int_literal(b"9223372036854775808"), Err(9223372036854775808.0));
        assert_eq!(parse_int_literal(b"100000000000000000000"), Err(1e20));
        assert_eq!(parse_int_literal(b"-100000000000000000000"), Err(-1e20));

        assert_eq!(parse_hex_literal(b"0x1F"), Ok(31));
        assert_eq!(parse_hex_literal(b"-0X10"), Ok(-16));
        assert_eq!(parse_hex_literal(b"0x7fffffffffffffff"), Ok(i64::MAX));
        assert_eq!(parse_hex_literal(b"-0x8000000000000000"), Ok(i64::MIN));
        assert_eq!(parse_hex_literal(b"0xffffffffffffffff"), Err(18446744073709551615.0));
        assert_eq!(parse_hex_literal(b"0x10000000000000000"), Err(18446744073709551616.0));
        assert_eq!(
            parse_hex_literal(b"0x1000000000000000000000000000000000"),
            Err(2f64.powi(132))
        );
    }
}
