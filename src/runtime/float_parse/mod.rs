//! Fast float parser based on github.com/lemire/fast_double_parser, but adopted to support AWK
//! semantics (no failures, just 0s and stopping early). Mistakes are surely my own.

fn is_integer(c: u8) -> bool {
    c.is_ascii_digit()
}

/// The simdjson repo has more optimizations to add for int parsing, but this is a big win over libc
/// for the time being, if only because we do not have to copy `s` into a NUL-terminated
/// representation.
pub fn strtoi(bs: &[u8]) -> i64 {
    if bs.is_empty() {
        return 0;
    }
    let neg = bs[0] == b'-';
    let off = if neg || bs[0] == b'+' { 1 } else { 0 };
    let mut i = 0i64;
    for b in bs[off..].iter().cloned().take_while(|b| is_integer(*b)) {
        let digit = (b - b'0') as i64;
        i = if let Some(i) = i.checked_mul(10).and_then(|i| i.checked_add(digit)) {
            i
        } else {
            // overflow
            return 0;
        }
    }
    if neg {
        -i
    } else {
        i
    }
}

/// Simple hexadecimal integer parser, similar in spirit to the strtoi implementation here.
pub fn hextoi(mut bs: &[u8]) -> i64 {
    let mut neg = false;
    if bs.is_empty() {
        return 0;
    }
    if bs[0] == b'-' {
        neg = true;
        bs = &bs[1..]
    }
    if bs.len() >= 2 && bs[0..2] == [b'0', b'x'] || bs[0..2] == [b'0', b'X'] {
        bs = &bs[2..]
    }
    let mut i = 0i64;
    for b in bs.iter().cloned() {
        let digit = match b {
            b'A'..=b'F' => (b - b'A') as i64 + 10,
            b'a'..=b'f' => (b - b'a') as i64 + 10,
            b'0'..=b'9' => (b - b'0') as i64,
            _ => break,
        };
        i = if let Some(i) = i.checked_mul(16).and_then(|i| i.checked_add(digit)) {
            i
        } else {
            // overflow
            return 0;
        }
    }
    if neg {
        -i
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

/// Parse a floating-poing number from `bs`, returning 0 if one isn't there.
pub fn strtod(bs: &[u8]) -> f64 {
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
