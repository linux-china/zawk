//! Conversions of TSV-escaped text to typed values, shared by the typed writers (`-o parquet`,
//! `-o avro`): integers, floats, booleans, dates, times and timestamps, and the inference of the
//! type of an `AUTO` column from the first rows.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

pub(crate) const MICROS_PER_SEC: i64 = 1_000_000;
pub(crate) const SECS_PER_DAY: i64 = 86_400;

/// The type of an `AUTO` column, inferred from its values (see [`infer_type`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Inferred {
    String,
    Int64,
    Double,
    Bool,
    Date,
}

/// Reverses the TSV escaping of `print` under `-o tsv`.
pub(crate) fn unescape_tsv(raw: &[u8]) -> std::borrow::Cow<'_, str> {
    match String::from_utf8_lossy(raw) {
        std::borrow::Cow::Borrowed(text) => crate::runtime::str_escape::unescape_tsv(text),
        std::borrow::Cow::Owned(text) => crate::runtime::str_escape::unescape_tsv(&text)
            .into_owned()
            .into(),
    }
}

/// An integer from an integer literal, or from a float literal with no fractional part (`1e3`,
/// `2.0`). Out-of-range values and values with a fractional part are invalid (`None`) rather
/// than saturated or truncated.
/// An integer value: a decimal integer, or a decimal number without a fractional part (`1e3`,
/// `-2.0`, `1.5e1`). The text is converted exactly, without going through `f64`, so values just
/// outside the range (`-9223372036854775809`) or with more digits than `f64` keeps
/// (`9007199254740993.0`) are not rounded into a different integer: out of range or fractional
/// values are invalid (`None`).
pub(crate) fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        return Some(i);
    }
    let (negative, body) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    let (mantissa, exp) = match body.find(['e', 'E']) {
        Some(pos) => (&body[..pos], body[pos + 1..].parse::<i32>().ok()?),
        None => (body, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let all_digits = |p: &str| p.bytes().all(|b| b.is_ascii_digit());
    if int_part.len() + frac_part.len() == 0 || !all_digits(int_part) || !all_digits(frac_part) {
        return None;
    }
    // The value is `digits * 10^exp` with all the digits of the mantissa, trailing zeros removed.
    let digits = format!("{}{}", int_part, frac_part);
    let digits = digits.trim_start_matches('0');
    let trimmed = digits.trim_end_matches('0');
    let exp = i64::from(exp) - frac_part.len() as i64 + (digits.len() - trimmed.len()) as i64;
    if trimmed.is_empty() {
        return Some(0);
    }
    // A negative exponent left means a fractional part; 19 digits or more cannot fit an i64
    // after scaling unless they already do, so the bounds keep the arithmetic below in i128.
    if exp < 0 || trimmed.len() + exp as usize > 19 {
        return None;
    }
    let mut value: i128 = trimmed.parse().ok()?;
    value = value.checked_mul(10i128.checked_pow(exp as u32)?)?;
    i64::try_from(if negative { -value } else { value }).ok()
}

/// A `FLOAT`/`DOUBLE` value. `inf`, `-inf` and `nan` written as such are kept as IEEE values,
/// but a finite number that overflows the type (`1e309`) is invalid instead of becoming infinite.
pub(crate) fn parse_float(s: &str) -> Option<f64> {
    let t = s.trim();
    let f = t.parse::<f64>().ok()?;
    (f.is_finite() || explicit_non_finite(t)).then_some(f)
}

/// Whether `t` spells a non-finite value (`inf`, `infinity`, `nan`, any case, with a sign).
pub(crate) fn explicit_non_finite(t: &str) -> bool {
    let t = t.trim_start_matches(['+', '-']);
    ["inf", "infinity", "nan"]
        .iter()
        .any(|w| t.eq_ignore_ascii_case(w))
}

/// A `FLOAT` value: as `parse_float`, and a finite value that overflows `f32` is invalid.
pub(crate) fn parse_f32(s: &str) -> Option<f32> {
    let f = parse_float(s)?;
    let g = f as f32;
    (g.is_finite() || !f.is_finite()).then_some(g)
}

/// A `BOOLEAN` value: `true/t/yes/y/on/✓` and nonzero numbers are true, `false/f/no/n/off/𐄂` and
/// zero are false (words in any case, surrounding whitespace ignored). Anything else, such as
/// `maybe` or `NaN`, is invalid (`None`).
pub(crate) fn parse_parquet_bool(s: &str) -> Option<bool> {
    let t = s.trim();
    match t.to_ascii_lowercase().as_str() {
        "true" | "t" | "yes" | "y" | "on" | "✓" => Some(true),
        "false" | "f" | "no" | "n" | "off" | "𐄂" => Some(false),
        _ => parse_float(t).filter(|f| !f.is_nan()).map(|f| f != 0.0),
    }
}

/// Microseconds since the Unix epoch from a numeric Unix timestamp: seconds, or milliseconds,
/// microseconds or nanoseconds when the value is too large to be a timestamp in seconds.
pub(crate) fn epoch_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(i) = s.parse::<i64>() {
        let abs = i.unsigned_abs();
        return if abs >= 100_000_000_000_000_000 {
            Some(i / 1000)
        } else if abs >= 100_000_000_000_000 {
            Some(i)
        } else if abs >= 100_000_000_000 {
            i.checked_mul(1000)
        } else {
            i.checked_mul(MICROS_PER_SEC)
        };
    }
    let f = s.parse::<f64>().ok().filter(|f| f.is_finite())?;
    let micros = (f * MICROS_PER_SEC as f64).round();
    (micros.abs() < i64::MAX as f64).then(|| micros as i64)
}

const DATE_TIME_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y-%m-%dT%H:%M",
    "%Y/%m/%d %H:%M:%S%.f",
    "%Y/%m/%d %H:%M",
];
const DATE_TIME_TZ_FORMATS: &[&str] = &["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f %z"];
const DATE_FORMATS: &[&str] = &["%Y-%m-%d", "%Y/%m/%d"];

/// Microseconds since the Unix epoch from a date-time or date string; naive values are UTC.
pub(crate) fn parse_date_time_str(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp_micros());
    }
    for fmt in DATE_TIME_TZ_FORMATS {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Some(dt.timestamp_micros());
        }
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Some(dt.timestamp_micros());
    }
    for fmt in DATE_TIME_FORMATS {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt.and_utc().timestamp_micros());
        }
    }
    for fmt in DATE_FORMATS {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp_micros());
        }
    }
    None
}

pub(crate) fn parse_timestamp(s: &str) -> Option<i64> {
    epoch_micros(s).or_else(|| parse_date_time_str(s))
}

/// Days since the Unix epoch from a date or date-time string. Eight digits are `YYYYMMDD`; any
/// other bare number (such as `2024` or a Unix timestamp) is invalid, not a timestamp in seconds.
pub(crate) fn parse_date(s: &str) -> Option<i32> {
    let s = s.trim();
    if s.bytes().all(|b| b.is_ascii_digit()) {
        let d = NaiveDate::parse_from_str(s, "%Y%m%d")
            .ok()
            .filter(|_| s.len() == 8)?;
        return i32::try_from(
            d.signed_duration_since(DateTime::UNIX_EPOCH.date_naive())
                .num_days(),
        )
        .ok();
    }
    let micros = parse_date_time_str(s)?;
    i32::try_from(micros.div_euclid(MICROS_PER_SEC * SECS_PER_DAY)).ok()
}

/// Microseconds since midnight.
pub(crate) fn parse_time(s: &str) -> Option<i64> {
    let t = s.trim();
    if let Some(f) = parse_float(t) {
        let micros = (f * MICROS_PER_SEC as f64).round();
        return (0.0..(SECS_PER_DAY * MICROS_PER_SEC) as f64)
            .contains(&micros)
            .then(|| micros as i64);
    }
    for fmt in ["%H:%M:%S%.f", "%H:%M"] {
        if let Ok(time) = NaiveTime::parse_from_str(t, fmt) {
            return Some(
                time.num_seconds_from_midnight() as i64 * MICROS_PER_SEC
                    + (time.nanosecond() / 1000) as i64,
            );
        }
    }
    parse_date_time_str(t).map(|micros| micros.rem_euclid(SECS_PER_DAY * MICROS_PER_SEC))
}

/// Infers the type of column `index` of an `AUTO` column from the sampled rows. Ignoring empty
/// and null values, it is `Int64` if every value is an integer as JSON writes it (`007` is not),
/// `Double` if every value is such a number, `Bool` if every value is `true` or `false`, `Date` if
/// every value is `YYYY-MM-DD`, and `String` otherwise.
pub(crate) fn infer_type(rows: &[Vec<u8>], index: usize) -> Inferred {
    use crate::runtime::writers::{json_bool, json_number};
    let (mut int, mut num, mut boolean, mut date, mut any) = (true, true, true, true, false);
    for row in rows {
        let Some(raw) = row.split(|b| *b == b'\t').nth(index) else {
            continue;
        };
        if crate::runtime::str_escape::is_tsv_null(raw) {
            continue;
        }
        let text = unescape_tsv(raw);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        any = true;
        match json_number(text) {
            Some(true) if int && text.parse::<i64>().is_ok() => {}
            Some(_) if text.parse::<f64>().is_ok_and(f64::is_finite) => int = false,
            _ => (int, num) = (false, false),
        }
        boolean &= json_bool(text).is_some();
        date &= text.len() == 10 && NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok();
        if !(int || num || boolean || date) {
            return Inferred::String;
        }
    }
    match () {
        _ if !any => Inferred::String,
        _ if int => Inferred::Int64,
        _ if num => Inferred::Double,
        _ if boolean => Inferred::Bool,
        _ if date => Inferred::Date,
        _ => Inferred::String,
    }
}
