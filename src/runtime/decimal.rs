//! Exact decimal arithmetic for the `dec_*` builtins, e.g. for money columns.
//!
//! awk numbers are `f64`, so `0.1 + 0.2` is `0.30000000000000004`. The `dec_*` functions take
//! decimal text and return decimal text, computing with [`rust_decimal`] (96-bit mantissa, 28
//! significant digits) in between, so no value goes through a float.
//!
//! Inputs: optional blanks and sign, digits with an optional fraction and exponent (`1.5e3`), and
//! integer digits optionally grouped by thousands with `,` or `_` (one kind per number), as
//! `strtonum` reads them. An empty string is 0, so that `total = dec_add(total, $3)` needs no
//! initialization. Any other text, a division by zero or an overflow returns `""` (`0` for
//! `dec_cmp`) with a warning, and sets `ERRNO`.

use rust_decimal::{Decimal, RoundingStrategy};
use std::cmp::Ordering;
use std::str::FromStr;

use crate::runtime::Int;

/// The `dec_*` functions returning text. Every one takes four string arguments, the missing
/// optional ones being `""` (see `cfg.rs`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DecOp {
    /// `dec_add(a, b)`
    Add,
    /// `dec_sub(a, b)`
    Sub,
    /// `dec_mul(a, b)`
    Mul,
    /// `dec_div(a, b[, scale[, mode]])`
    Div,
    /// `dec_round(a[, scale[, mode]])`
    Round,
    /// `dec_format(a[, scale[, mode]])`
    Format,
}

impl DecOp {
    pub fn func_name(self) -> &'static str {
        match self {
            DecOp::Add => "dec_add",
            DecOp::Sub => "dec_sub",
            DecOp::Mul => "dec_mul",
            DecOp::Div => "dec_div",
            DecOp::Round => "dec_round",
            DecOp::Format => "dec_format",
        }
    }

    /// The number of arguments given by the script: the minimum and the maximum.
    pub fn arg_range(self) -> (usize, usize) {
        match self {
            DecOp::Add | DecOp::Sub | DecOp::Mul => (2, 2),
            DecOp::Div => (2, 4),
            DecOp::Round | DecOp::Format => (1, 3),
        }
    }

    /// The code passed to the compiled `decimal` intrinsic.
    pub fn code(self) -> Int {
        self as Int
    }

    pub fn from_code(code: Int) -> Option<DecOp> {
        [
            DecOp::Add,
            DecOp::Sub,
            DecOp::Mul,
            DecOp::Div,
            DecOp::Round,
            DecOp::Format,
        ]
        .into_iter()
        .find(|op| op.code() == code)
    }
}

/// Evaluate `op` on its four arguments (see [`DecOp`] for their meaning); `""` on an error.
pub fn eval(op: DecOp, a: &str, b: &str, c: &str, d: &str) -> String {
    let func = op.func_name();
    match try_eval(op, a, b, c, d) {
        Ok(text) => text,
        Err(msg) => {
            crate::runtime::stdlib_warning(func, msg);
            String::new()
        }
    }
}

/// `dec_cmp(a, b)`: -1, 0 or 1 as `a` is less than, equal to or greater than `b`; 0 on an error.
pub fn cmp(a: &str, b: &str) -> Int {
    match (parse(a), parse(b)) {
        (Ok(a), Ok(b)) => match a.cmp(&b) {
            Ordering::Less => -1,
            Ordering::Equal => 0,
            Ordering::Greater => 1,
        },
        (Err(msg), _) | (_, Err(msg)) => {
            crate::runtime::stdlib_warning("dec_cmp", msg);
            0
        }
    }
}

fn try_eval(op: DecOp, a: &str, b: &str, c: &str, d: &str) -> Result<String, String> {
    let x = parse(a)?;
    let res = match op {
        DecOp::Add => x.checked_add(parse(b)?).ok_or_else(overflow)?,
        DecOp::Sub => x.checked_sub(parse(b)?).ok_or_else(overflow)?,
        DecOp::Mul => x.checked_mul(parse(b)?).ok_or_else(overflow)?,
        DecOp::Div => {
            let y = parse(b)?;
            if y.is_zero() {
                return Err(format!("division by zero: {:?} / {:?}", a, b));
            }
            let q = x.checked_div(y).ok_or_else(overflow)?;
            match parse_scale(c)? {
                Some(scale) => round(q, scale, parse_mode(d)?)?,
                None => q.normalize(),
            }
        }
        DecOp::Round => round(x, parse_scale(b)?.unwrap_or(0), parse_mode(c)?)?,
        DecOp::Format => {
            let x = match parse_scale(b)? {
                Some(scale) => round(x, scale, parse_mode(c)?)?,
                None => x,
            };
            return Ok(group_thousands(&to_text(x)));
        }
    };
    Ok(to_text(res))
}

fn overflow() -> String {
    "overflow: the result exceeds 28 significant digits".to_string()
}

/// Round to `scale` fractional digits, padding with zeros: `round(3, 2)` is `3.00`. A negative
/// scale rounds to the left of the decimal point: `round(1250, -2)` is `1300`. An overflow error
/// when the padded value exceeds 28 significant digits (`rescale` would silently keep a smaller
/// scale).
fn round(x: Decimal, scale: i32, mode: RoundingStrategy) -> Result<Decimal, String> {
    if scale >= 0 {
        let scale = scale as u32;
        let mut r = x.round_dp_with_strategy(scale, mode);
        r.rescale(scale);
        if r.scale() != scale {
            return Err(overflow());
        }
        return Ok(r);
    }
    let unit = Decimal::from_i128_with_scale(10i128.pow(scale.unsigned_abs()), 0);
    let r = (x / unit).round_dp_with_strategy(0, mode);
    r.checked_mul(unit).ok_or_else(overflow)
}

/// The text of a decimal, without a sign for zero (`-0.00` is `0.00`).
fn to_text(mut x: Decimal) -> String {
    if x.is_zero() {
        x.set_sign_positive(true);
    }
    x.to_string()
}

/// Largest number of fractional digits of a decimal.
const MAX_SCALE: u32 = 28;

/// A scale argument: `None` for `""`, else an integer from -28 to 28.
fn parse_scale(s: &str) -> Result<Option<i32>, String> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let max = MAX_SCALE as i64;
    match t.parse::<i64>() {
        Ok(n) if (-max..=max).contains(&n) => Ok(Some(n as i32)),
        _ => Err(format!(
            "invalid scale {:?}: expected an integer from -{} to {}",
            s, MAX_SCALE, MAX_SCALE
        )),
    }
}

/// A rounding mode argument; `""` is `half_up`.
fn parse_mode(s: &str) -> Result<RoundingStrategy, String> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "" | "half_up" => RoundingStrategy::MidpointAwayFromZero,
        "half_even" => RoundingStrategy::MidpointNearestEven,
        "half_down" => RoundingStrategy::MidpointTowardZero,
        "up" => RoundingStrategy::AwayFromZero,
        "down" => RoundingStrategy::ToZero,
        "ceil" => RoundingStrategy::ToPositiveInfinity,
        "floor" => RoundingStrategy::ToNegativeInfinity,
        _ => {
            return Err(format!(
                "invalid rounding mode {:?}: expected half_up, half_even, half_down, up, down, ceil or floor",
                s
            ));
        }
    })
}

/// Parse decimal text, see the module documentation.
pub(crate) fn parse(s: &str) -> Result<Decimal, String> {
    let invalid = || format!("invalid decimal {:?}", s);
    let t = s.trim();
    if t.is_empty() {
        return Ok(Decimal::ZERO);
    }
    let (neg, body) = match t.as_bytes()[0] {
        b'-' => (true, &t[1..]),
        b'+' => (false, &t[1..]),
        _ => (false, t),
    };
    // Split off the exponent, then the fraction.
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(i) => (&body[..i], Some(&body[i + 1..])),
        None => (body, None),
    };
    let (int_part, frac) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], Some(&mantissa[i + 1..])),
        None => (mantissa, None),
    };
    let int_digits = ungroup(int_part).ok_or_else(invalid)?;
    let frac = frac.unwrap_or("");
    if (int_digits.is_empty() && frac.is_empty())
        || !int_digits.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    let mut text = String::with_capacity(t.len());
    if neg {
        text.push('-');
    }
    text.push_str(if int_digits.is_empty() {
        "0"
    } else {
        &int_digits
    });
    if !frac.is_empty() {
        text.push('.');
        text.push_str(frac);
    }
    let parsed = match exp {
        Some(e) => {
            let e = e.strip_prefix('+').unwrap_or(e);
            let digits = e.strip_prefix('-').unwrap_or(e);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return Err(invalid());
            }
            text.push('e');
            text.push_str(e);
            Decimal::from_scientific(&text)
        }
        None => Decimal::from_str(&text),
    };
    parsed.map_err(|_| format!("invalid decimal {:?}: out of range", s))
}

/// Integer digits without thousands separators: `1,234,567` and `1_234` are grouped by threes,
/// with one kind of separator. `None` if the grouping is invalid.
pub(crate) fn ungroup(int_part: &str) -> Option<std::borrow::Cow<'_, str>> {
    let sep = match (int_part.contains(','), int_part.contains('_')) {
        (false, false) => return Some(int_part.into()),
        (true, true) => return None,
        (true, false) => ',',
        (false, true) => '_',
    };
    let mut groups = int_part.split(sep);
    let first = groups.next()?;
    if first.is_empty() || first.len() > 3 {
        return None;
    }
    let mut digits = String::from(first);
    for g in groups {
        if g.len() != 3 {
            return None;
        }
        digits.push_str(g);
    }
    Some(digits.into())
}

/// Group the integer digits of decimal text by thousands with `,`: `-1234567.5` is
/// `-1,234,567.5`.
fn group_thousands(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(r) => ("-", r),
        None => ("", text),
    };
    let (int_part, frac) = match rest.find('.') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let mut out = String::with_capacity(text.len() + int_part.len() / 3);
    out.push_str(sign);
    for (i, ch) in int_part.chars().enumerate() {
        if i > 0 && (int_part.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.push_str(frac);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(op: DecOp, a: &str, b: &str, c: &str, d: &str) -> String {
        try_eval(op, a, b, c, d).unwrap_or_else(|e| format!("ERR {}", e))
    }

    #[test]
    fn add_sub_mul_are_exact() {
        assert_eq!(ev(DecOp::Add, "0.1", "0.2", "", ""), "0.3");
        assert_eq!(ev(DecOp::Add, "1.10", "2.20", "", ""), "3.30");
        assert_eq!(ev(DecOp::Add, "", "12.50", "", ""), "12.50");
        assert_eq!(ev(DecOp::Sub, "1.00", "0.99", "", ""), "0.01");
        assert_eq!(ev(DecOp::Sub, "0.10", "0.10", "", ""), "0.00");
        assert_eq!(ev(DecOp::Mul, "19.99", "3", "", ""), "59.97");
        assert_eq!(ev(DecOp::Mul, "-1.10", "1.10", "", ""), "-1.2100");
    }

    #[test]
    fn division() {
        assert_eq!(ev(DecOp::Div, "10", "4", "", ""), "2.5");
        assert_eq!(ev(DecOp::Div, "10", "3", "2", ""), "3.33");
        assert_eq!(ev(DecOp::Div, "2", "3", "2", ""), "0.67");
        assert_eq!(ev(DecOp::Div, "2", "3", "2", "down"), "0.66");
        assert_eq!(
            ev(DecOp::Div, "1", "3", "", ""),
            "0.3333333333333333333333333333"
        );
        assert!(ev(DecOp::Div, "1", "0", "", "").starts_with("ERR division by zero"));
    }

    #[test]
    fn rounding_modes() {
        assert_eq!(ev(DecOp::Round, "2.675", "2", "", ""), "2.68");
        assert_eq!(ev(DecOp::Round, "2.665", "2", "half_even", ""), "2.66");
        assert_eq!(ev(DecOp::Round, "2.675", "2", "half_even", ""), "2.68");
        assert_eq!(ev(DecOp::Round, "2.665", "2", "half_down", ""), "2.66");
        assert_eq!(ev(DecOp::Round, "-2.5", "", "", ""), "-3");
        assert_eq!(ev(DecOp::Round, "2.001", "2", "up", ""), "2.01");
        assert_eq!(ev(DecOp::Round, "2.009", "2", "down", ""), "2.00");
        assert_eq!(ev(DecOp::Round, "-2.001", "2", "ceil", ""), "-2.00");
        assert_eq!(ev(DecOp::Round, "-2.001", "2", "floor", ""), "-2.01");
        assert_eq!(ev(DecOp::Round, "3", "2", "", ""), "3.00");
        assert_eq!(ev(DecOp::Round, "-0.001", "2", "", ""), "0.00");
        assert_eq!(ev(DecOp::Round, "1250", "-2", "", ""), "1300");
        assert_eq!(ev(DecOp::Round, "-1250", "-2", "half_even", ""), "-1200");
        assert_eq!(ev(DecOp::Round, "1234.5", "-2", "", ""), "1200");
        assert!(ev(DecOp::Round, "1", "29", "", "").starts_with("ERR invalid scale"));
        assert!(ev(DecOp::Round, "1", "1.5", "", "").starts_with("ERR invalid scale"));
        assert!(ev(DecOp::Round, "1", "2", "nearest", "").starts_with("ERR invalid rounding"));
    }

    #[test]
    fn format() {
        assert_eq!(ev(DecOp::Format, "1234567.5", "2", "", ""), "1,234,567.50");
        assert_eq!(ev(DecOp::Format, "-1234.567", "", "", ""), "-1,234.567");
        assert_eq!(ev(DecOp::Format, "999", "", "", ""), "999");
        assert_eq!(ev(DecOp::Format, "1000", "0", "", ""), "1,000");
        assert_eq!(ev(DecOp::Format, "0.125", "2", "down", ""), "0.12");
    }

    #[test]
    fn parsing() {
        assert_eq!(parse(" +1,234.50 ").unwrap().to_string(), "1234.50");
        assert_eq!(parse("1_234_567").unwrap().to_string(), "1234567");
        assert_eq!(parse("1.5e3").unwrap().to_string(), "1500");
        assert_eq!(parse("2.5E-2").unwrap().to_string(), "0.025");
        assert_eq!(parse(".5").unwrap().to_string(), "0.5");
        assert_eq!(parse("5.").unwrap().to_string(), "5");
        assert_eq!(parse("-0.0").unwrap().is_zero(), true);
        for bad in [
            "abc",
            "1,23",
            "1,234_567",
            "12,34.5",
            "1.2.3",
            "--1",
            "1e",
            "0x10",
            ".",
            "inf",
            "1 2",
        ] {
            assert!(parse(bad).is_err(), "{:?} should be invalid", bad);
        }
    }

    #[test]
    fn overflow_is_an_error() {
        let big = "79228162514264337593543950335";
        assert!(ev(DecOp::Add, big, "1", "", "").starts_with("ERR overflow"));
        assert!(ev(DecOp::Mul, big, "2", "", "").starts_with("ERR overflow"));
        // No room for the padding zeros: an error, not a smaller scale.
        assert!(ev(DecOp::Round, big, "2", "", "").starts_with("ERR overflow"));
        assert!(ev(DecOp::Format, big, "1", "", "").starts_with("ERR overflow"));
        assert!(ev(DecOp::Div, big, "1", "2", "").starts_with("ERR overflow"));
        assert_eq!(ev(DecOp::Round, big, "0", "", ""), big);
        assert_eq!(
            ev(DecOp::Round, "1", "28", "", ""),
            "1.0000000000000000000000000000"
        );
        assert!(ev(DecOp::Round, "10", "28", "", "").starts_with("ERR overflow"));
    }

    #[test]
    fn fractional_digits_beyond_28_are_rounded() {
        let tiny = "0.0000000000000001";
        assert_eq!(
            ev(DecOp::Mul, tiny, tiny, "", ""),
            "0.0000000000000000000000000000"
        );
        assert!(ev(DecOp::Add, "1e-30", "1", "", "").contains("out of range"));
    }

    #[test]
    fn compare() {
        assert_eq!(cmp("11", "5"), 1);
        assert_eq!(cmp("1.10", "1.1"), 0);
        assert_eq!(cmp("-1", "0"), -1);
        assert_eq!(cmp("", "0"), 0);
    }

    #[test]
    fn op_codes_round_trip() {
        for op in [
            DecOp::Add,
            DecOp::Sub,
            DecOp::Mul,
            DecOp::Div,
            DecOp::Round,
            DecOp::Format,
        ] {
            assert_eq!(DecOp::from_code(op.code()), Some(op));
        }
        assert_eq!(DecOp::from_code(99), None);
    }
}
