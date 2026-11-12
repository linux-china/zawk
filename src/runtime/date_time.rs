use crate::runtime;
use crate::runtime::{Int, Str, StrMap};
use chrono::{
    DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone,
    Timelike, Utc,
};
use std::sync::LazyLock;
use std::time::SystemTime;

const WEEKS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// Convert a unix timestamp to local date time, falling back to the epoch for out-of-range values.
fn local_date_time(timestamp: i64) -> DateTime<Local> {
    let utc = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
    utc.with_timezone(&Local)
}

/// Default `strftime` format, same as `PROCINFO["strftime"]`.
pub const DEFAULT_STRFTIME_FORMAT: &str = "%a %b %e %H:%M:%S %Z %Y";

/// Flags of [`awk_strftime`]: format in UTC rather than local time (the third argument of
/// `strftime`), use `PROCINFO["strftime"]` as the format (`strftime()` without arguments), and
/// format the current time because no timestamp is given (`strftime(format)`). The omitted
/// timestamp is a flag rather than a reserved timestamp value, because every `Int` is a valid
/// timestamp argument (`i64::MIN` must give the out-of-range empty string, as in gawk).
pub const STRFTIME_UTC: Int = 1;
pub const STRFTIME_PROCINFO_FORMAT: Int = 2;
pub const STRFTIME_NOW: Int = 4;

/// `systime_ms()`: the current Unix time in milliseconds.
pub fn systime_ms() -> Int {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Int)
}

/// The time zone `strftime` formats a time in.
#[derive(Clone, Debug, PartialEq)]
enum Zone {
    Local,
    Utc,
    /// A time zone of the IANA database at the formatted time: its offset east of UTC in
    /// seconds, its abbreviation (`%Z`) and whether it is daylight saving time.
    Named {
        offset: i32,
        abbr: String,
        dst: bool,
    },
}

impl Zone {
    /// The time zone named `name` (`"Asia/Shanghai"`, `"UTC"`) at `timestamp`, if it is one.
    fn named(name: &str, timestamp: i64) -> Option<Zone> {
        // numbers are utc flags, not time zones: skip the lookup
        if name.is_empty() || !name.bytes().any(|b| b.is_ascii_alphabetic()) {
            return None;
        }
        let tz = jiff::tz::TimeZone::get(name).ok()?;
        let ts = jiff::Timestamp::from_second(timestamp).ok()?;
        let info = tz.to_offset_info(ts);
        Some(Zone::Named {
            offset: info.offset().seconds(),
            abbr: info.abbreviation().to_string(),
            dst: info.dst().is_dst(),
        })
    }
}

/// Format a unix timestamp as local date time. Unknown conversions are output as is, and a
/// timestamp out of range gives an empty string, as in gawk.
pub fn strftime(format: &str, timestamp: i64) -> String {
    strftime_tz(format, timestamp, &Zone::Local)
}

/// `strftime` with the C library, which knows time zone abbreviations (`%Z`), the `E`/`O`
/// modifiers and the platform's other extensions.
#[cfg(unix)]
fn strftime_tz(format: &str, timestamp: i64, zone: &Zone) -> String {
    use std::sync::Once;
    unsafe extern "C" {
        fn tzset();
    }
    static TZSET: Once = Once::new();
    // time_t is 32-bit on some platforms
    #[allow(irrefutable_let_patterns)]
    let Ok(time) = libc::time_t::try_from(timestamp) else {
        return String::new();
    };
    // the format stops at an embedded NUL, as in C
    let format = format.split('\0').next().unwrap_or_default();
    if format.is_empty() {
        return String::new();
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // the abbreviation of a named zone, which `tm.tm_zone` points to while formatting
    let mut zone_abbr = None;
    let tm_ptr = unsafe {
        match zone {
            Zone::Utc => libc::gmtime_r(&time, &mut tm),
            Zone::Local => {
                TZSET.call_once(|| tzset());
                libc::localtime_r(&time, &mut tm)
            }
            // the broken-down time of the zone is the UTC one of the shifted time
            Zone::Named { offset, abbr, dst } => {
                let Some(shifted) = time.checked_add(*offset as libc::time_t) else {
                    return String::new();
                };
                let res = libc::gmtime_r(&shifted, &mut tm);
                let Ok(abbr) = std::ffi::CString::new(abbr.as_str()) else {
                    return String::new();
                };
                tm.tm_gmtoff = *offset as _;
                tm.tm_isdst = *dst as _;
                tm.tm_zone = abbr.as_ptr() as _;
                zone_abbr = Some(abbr);
                res
            }
        }
    };
    if tm_ptr.is_null() {
        return String::new();
    }
    let _zone_abbr = zone_abbr;
    // a trailing space tells an empty result (e.g. `%p` in some locales) from a too small buffer
    let Ok(c_format) = std::ffi::CString::new(format!("{format} ")) else {
        return String::new();
    };
    let mut size = 128 + format.len() * 4;
    while size <= 1 << 20 {
        let mut buf = vec![0u8; size];
        let len = unsafe { libc::strftime(buf.as_mut_ptr().cast(), size, c_format.as_ptr(), &tm) };
        if len > 0 {
            buf.truncate(len - 1);
            return String::from_utf8_lossy(&buf).into_owned();
        }
        size *= 2;
    }
    String::new()
}

/// `strftime` with chrono: format item by item, so that an unknown conversion is output as is
/// rather than failing the whole format.
#[cfg(not(unix))]
fn strftime_tz(format: &str, timestamp: i64, zone: &Zone) -> String {
    use chrono::format::{Item, StrftimeItems};
    use std::fmt::Write;
    let Some(utc_date_time) = DateTime::from_timestamp(timestamp, 0) else {
        return String::new();
    };
    let local_date_time = utc_date_time.with_timezone(&Local);
    let named_date_time = match zone {
        Zone::Named { offset, .. } => {
            FixedOffset::east_opt(*offset).map(|offset| utc_date_time.with_timezone(&offset))
        }
        _ => None,
    };
    let mut text = String::new();
    let mut rest = format;
    while !rest.is_empty() {
        // a literal run, or one conversion with its flags/modifiers
        let len = match rest.find('%') {
            Some(0) => {
                let spec = &rest[1..];
                let skip = spec
                    .find(|c: char| {
                        !matches!(
                            c,
                            '-' | '_' | '0' | '^' | '#' | ':' | '.' | '1'..='9' | 'E' | 'O'
                        )
                    })
                    .unwrap_or(spec.len());
                1 + skip + spec[skip..].chars().next().map_or(0, char::len_utf8)
            }
            Some(n) => n,
            None => rest.len(),
        };
        let item = &rest[..len];
        rest = &rest[len..];
        // chrono does not know the E/O modifiers: drop them
        let item_format = if item.starts_with('%') {
            item.replace(['E', 'O'], "")
        } else {
            item.to_string()
        };
        if StrftimeItems::new(&item_format).any(|item| item == Item::Error) {
            text.push_str(item);
        } else if let (Zone::Named { abbr, .. }, "%Z") = (zone, item_format.as_str()) {
            text.push_str(abbr);
        } else if let Some(named) = &named_date_time {
            let _ = write!(text, "{}", named.format(&item_format));
        } else if *zone == Zone::Utc {
            let _ = write!(text, "{}", utc_date_time.format(&item_format));
        } else {
            let _ = write!(text, "{}", local_date_time.format(&item_format));
        }
    }
    text
}

/// awk's `strftime(format, timestamp, utc)`, with the arguments filled in by the compiler: the
/// timestamp is ignored when it is not given, `flags` holds [`STRFTIME_UTC`],
/// [`STRFTIME_PROCINFO_FORMAT`] and [`STRFTIME_NOW`], and `timezone` is the utc argument as text. When the utc flag
/// is set and `timezone` names a time zone of the IANA database (`"Asia/Shanghai"`), the time is
/// formatted in that zone; any other true utc argument means UTC, as in gawk. An empty format
/// gives an empty string, as in gawk.
pub(crate) fn awk_strftime(
    procinfo: &StrMap<Str>,
    format: &Str,
    timestamp: Int,
    flags: Int,
    timezone: &Str,
) -> String {
    let format = if flags & STRFTIME_PROCINFO_FORMAT != 0 {
        let key = Str::from("strftime");
        if procinfo.contains(&key) {
            procinfo.get(&key).to_string()
        } else {
            DEFAULT_STRFTIME_FORMAT.to_string()
        }
    } else {
        format.to_string()
    };
    let timestamp = if flags & STRFTIME_NOW != 0 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    } else {
        timestamp
    };
    let zone = if flags & STRFTIME_UTC == 0 {
        Zone::Local
    } else {
        timezone
            .with_bytes(|bs| {
                std::str::from_utf8(bs)
                    .ok()
                    .and_then(|n| Zone::named(n, timestamp))
            })
            .unwrap_or(Zone::Utc)
    };
    strftime_tz(&format, timestamp, &zone)
}

/// Sentinel timezone for `mktime(text [, utc])`: no UTC offset is given, so text without an
/// explicit offset is parsed as local time, or as UTC when the utc flag is set.
pub const MKTIME_LOCAL_TIMEZONE: i64 = i64::MIN;

/// awk's `mktime(text [, utc [, timezone]])`: parse date time text to a unix timestamp, `-1` if
/// the text cannot be parsed (as in gawk).
/// Text without an explicit offset is parsed in `timezone`, the UTC offset in hours (e.g. `8` for
/// UTC+8, `-5` for UTC-5); when it is [`MKTIME_LOCAL_TIMEZONE`] or out of range, in UTC if `utc` is
/// nonzero (gawk's utc-flag) and in local time otherwise.
pub fn mktime(date_time_text: &str, utc: bool, timezone: i64) -> i64 {
    let offset = timezone
        .checked_mul(3600)
        .and_then(|seconds| i32::try_from(seconds).ok())
        .and_then(FixedOffset::east_opt);
    match offset {
        Some(offset) => mktime_tz(date_time_text, &offset),
        None if utc => mktime_tz(date_time_text, &Utc),
        None => mktime_tz(date_time_text, &Local),
    }
    .unwrap_or(-1)
}

fn mktime_tz<Tz: TimeZone>(date_time_text: &str, tz: &Tz) -> Option<i64> {
    // gawk format first, so that the lenient parsers below cannot misread it
    if let Some(naive) =
        parse_gawk_date_time(date_time_text).or_else(|| parse_iso_local_date_time(date_time_text))
    {
        return local_timestamp(tz, &naive);
    }
    if let Some(timestamp) = chrono_systemd_time::parse_timestamp_tz(date_time_text, tz.clone())
        .ok()
        .and_then(|x| x.single())
    {
        return Some(timestamp.timestamp());
    }
    // date-only text means midnight: dateparser would otherwise fill in the current time
    if let Ok(date_time) = dateparser::parse_with(date_time_text, tz, NaiveTime::MIN) {
        return Some(date_time.timestamp());
    }
    // fend date format: Thursday, 20 May 2021
    if let Some(space) = date_time_text
        .find(' ')
        .filter(|_| is_fend_date(date_time_text))
    {
        let adjusted_dt_text = &date_time_text[space + 1..];
        if let Ok(date_time) = dateparser::parse_with(adjusted_dt_text, tz, NaiveTime::MIN) {
            return Some(date_time.timestamp());
        }
    }
    None
}

/// Unix timestamp of a local date time in `tz`. An ambiguous time (DST fall back) takes the
/// earlier instant; a nonexistent time (in the DST spring forward gap) uses the UTC offset in
/// effect before the transition, as C's `mktime` does, e.g. 02:30 EST on the day New York
/// springs forward.
fn local_timestamp<Tz: TimeZone>(tz: &Tz, naive: &NaiveDateTime) -> Option<i64> {
    // The offsets a day before and after bracket any transition, since zones never shift twice
    // within a day. They are resolved here rather than by `from_local_datetime`, because chrono's
    // `Local` does not report fall back times as ambiguous and returns the later instant.
    let offset_at = |days: i64| {
        let local = naive.checked_add_signed(chrono::TimeDelta::days(days))?;
        Some(tz.offset_from_local_datetime(&local).earliest()?.fix())
    };
    let before = offset_at(-1);
    let after = offset_at(1);
    // timestamp of `naive` read with `offset`, if `tz` really has that offset at that instant
    let valid = |offset: FixedOffset| {
        let utc = *naive - offset;
        (tz.offset_from_utc_datetime(&utc).fix() == offset).then(|| utc.and_utc().timestamp())
    };
    let candidates = [
        before,
        after,
        tz.offset_from_local_datetime(naive)
            .earliest()
            .map(|o| o.fix()),
    ];
    candidates
        .into_iter()
        .flatten()
        .filter_map(valid)
        .min()
        .or_else(|| before.map(|offset| (*naive - offset).and_utc().timestamp()))
}

/// gawk's `"YYYY MM DD HH MM SS [DST]"`: 6 or 7 integers separated by whitespace. Out-of-range
/// values are normalized as in gawk, e.g. month `13` is January of the next year. The DST field
/// is ignored, since the time zone decides it.
fn parse_gawk_date_time(text: &str) -> Option<NaiveDateTime> {
    let fields = text
        .split_ascii_whitespace()
        .map(|field| field.parse::<i64>().ok())
        .collect::<Option<Vec<i64>>>()?;
    let [year, month, day, hour, minute, second] = match fields.len() {
        6 | 7 => [
            fields[0], fields[1], fields[2], fields[3], fields[4], fields[5],
        ],
        _ => return None,
    };
    let months = year.checked_mul(12)?.checked_add(month.checked_sub(1)?)?;
    let year = i32::try_from(months.div_euclid(12)).ok()?;
    let month = months.rem_euclid(12) as u32 + 1;
    let seconds = day
        .checked_sub(1)?
        .checked_mul(86400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?;
    NaiveDate::from_ymd_opt(year, month, 1)?
        .and_hms_opt(0, 0, 0)?
        .checked_add_signed(chrono::TimeDelta::try_seconds(seconds)?)
}

/// ISO 8601 local date time without an offset, `YYYY-MM-DD[T ]HH:MM[:SS[.fraction]]`, e.g.
/// `2021-05-01T01:17:02` or `2021-05-01 01:17:02`. Parsed here rather than by systemd time or
/// dateparser so that [`local_timestamp`] handles times in a DST gap or overlap, which those
/// parsers reject.
fn parse_iso_local_date_time(text: &str) -> Option<NaiveDateTime> {
    let text = text.trim();
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(text, format).ok())
}

fn is_fend_date(text: &str) -> bool {
    match text.find(',') {
        Some(comma) => WEEKS.contains(&&text[0..comma]),
        None => false,
    }
}

pub(crate) fn datetime<'a>(date_time_text: &str) -> runtime::StrMap<'a, Int> {
    if date_time_text.is_empty() {
        let seconds = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        datetime2(seconds)
    } else if let Ok(timestamp) = date_time_text.parse::<i64>() {
        // all-digit text is a Unix timestamp, so "20240115" is not 2024-01-15
        datetime2(timestamp)
    } else {
        // mktime()'s -1 is also a valid timestamp, so parse here to tell failure apart
        match mktime_tz(date_time_text, &Local) {
            Some(timestamp) => datetime2(timestamp),
            None => {
                runtime::stdlib_warning(
                    "datetime",
                    format!("invalid date time {:?}", date_time_text),
                );
                runtime::StrMap::default()
            }
        }
    }
}

pub(crate) fn datetime2<'a>(timestamp: i64) -> runtime::StrMap<'a, Int> {
    let result: runtime::StrMap<Int> = runtime::StrMap::default();
    // use local time zone, same as strftime()
    let local_now = local_date_time(timestamp);
    result.insert(Str::from("second"), local_now.second() as Int);
    result.insert(Str::from("minute"), local_now.minute() as Int);
    result.insert(Str::from("hour"), local_now.hour() as Int);
    result.insert(Str::from("althour"), local_now.hour12().1 as Int);
    result.insert(Str::from("monthday"), local_now.day() as Int);
    result.insert(Str::from("month"), local_now.month() as Int);
    result.insert(Str::from("year"), local_now.year() as Int);
    // Sunday is 0, Monday is 1
    let mut weekday = local_now.weekday() as Int + 1;
    if weekday == 7 {
        weekday = 0;
    }
    result.insert(Str::from("weekday"), weekday);
    result.insert(Str::from("yearday"), local_now.ordinal() as Int);
    result.insert(
        Str::from("quarter"),
        ((local_now.month() - 1) / 3 + 1) as Int,
    );
    // last day of the month: 28-31
    let (next_year, next_month) = if local_now.month() == 12 {
        (local_now.year() + 1, 1)
    } else {
        (local_now.year(), local_now.month() + 1)
    };
    let month_lastday = NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|d| d.pred_opt())
        .map_or(local_now.day(), |d| d.day());
    result.insert(Str::from("month_lastday"), month_lastday as Int);
    // ISO 8601 week number and week-based year, same as strftime("%V") / strftime("%G")
    let iso_week = local_now.iso_week();
    result.insert(Str::from("week"), iso_week.week() as Int);
    result.insert(Str::from("isoyear"), iso_week.year() as Int);
    result
}

static DURATION_TOKEN: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d+(?:\.\d+)?)\s*([A-Za-z]+)").unwrap());

/// Map a time unit (Go-style short form or full word) to a unit fend understands.
/// `m` always means minutes in a duration context, never meters.
fn duration_unit(unit: &str) -> Option<&'static str> {
    match unit.to_ascii_lowercase().as_str() {
        "ms" | "msec" | "msecs" | "millisecond" | "milliseconds" => Some("ms"),
        "s" | "sec" | "secs" | "second" | "seconds" => Some("sec"),
        "m" | "min" | "mins" | "minute" | "minutes" => Some("min"),
        "h" | "hr" | "hrs" | "hour" | "hours" => Some("hour"),
        "d" | "day" | "days" => Some("day"),
        "w" | "week" | "weeks" => Some("week"),
        _ => None,
    }
}

/// Rewrite `1h30m`, `1h 30m`, `2min 12sec` into `(1 hour) + (30 min)` style fend expressions.
/// Adjacent time tokens (separated only by whitespace) are summed; other text is kept as-is.
fn normalize_duration(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut last = 0;
    let mut prev_token = false;
    for caps in DURATION_TOKEN.captures_iter(text) {
        let m = caps.get(0).unwrap();
        // skip tokens glued to a preceding identifier/number, e.g. `x1h`, but allow `1h30m`
        let glued = !(prev_token && m.start() == last)
            && text[..m.start()]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '.' || c == '_');
        let unit = match duration_unit(&caps[2]) {
            Some(unit) if !glued => unit,
            _ => {
                prev_token = false;
                continue;
            }
        };
        let between = &text[last..m.start()];
        if prev_token && between.trim().is_empty() {
            out.push_str(" + ");
        } else {
            out.push_str(between);
        }
        out.push_str(&format!("({} {})", &caps[1], unit));
        last = m.end();
        prev_token = true;
    }
    out.push_str(&text[last..]);
    out
}

pub fn duration(text: &str) -> Int {
    let expr = format!("({}) to ms", normalize_duration(text));
    let mut context = fend_core::Context::new();
    match fend_core::evaluate(&expr, &mut context) {
        Ok(result) => {
            let result = result.get_main_result();
            // "1500 ms", "approx. 33.3333 ms", ...: take the first number
            let duration_ms = result
                .split(' ')
                .find_map(|part| part.replace(',', "").parse::<f64>().ok());
            match duration_ms {
                Some(ms) => (ms / 1000.0).round() as Int,
                None => {
                    runtime::stdlib_warning("duration", format!("invalid duration {:?}", text));
                    0
                }
            }
        }
        Err(e) => {
            runtime::stdlib_warning("duration", e);
            0
        }
    }
}

/// Unit of `date_add`/`date_diff`/`date_trunc`: seconds, minutes and hours are fixed lengths of time; days and
/// weeks follow the local wall clock (a day is 23 or 25 hours across a DST change); months,
/// quarters and years follow the calendar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DateUnit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Quarter,
    Year,
}

impl DateUnit {
    fn parse(unit: &str) -> Option<Self> {
        let unit = match unit.trim().to_ascii_lowercase().as_str() {
            "s" | "sec" | "secs" | "second" | "seconds" => Self::Second,
            "m" | "min" | "mins" | "minute" | "minutes" => Self::Minute,
            "h" | "hr" | "hrs" | "hour" | "hours" => Self::Hour,
            "d" | "day" | "days" => Self::Day,
            "w" | "wk" | "week" | "weeks" => Self::Week,
            "mo" | "mon" | "month" | "months" => Self::Month,
            "q" | "quarter" | "quarters" => Self::Quarter,
            "y" | "yr" | "year" | "years" => Self::Year,
            _ => return None,
        };
        Some(unit)
    }

    /// Length in seconds of a fixed-length unit.
    fn seconds(self) -> Option<i64> {
        match self {
            Self::Second => Some(1),
            Self::Minute => Some(60),
            Self::Hour => Some(3600),
            _ => None,
        }
    }

    /// Length in months of a calendar unit.
    fn months(self) -> Option<i64> {
        match self {
            Self::Month => Some(1),
            Self::Quarter => Some(3),
            Self::Year => Some(12),
            _ => None,
        }
    }
}

fn local_naive(timestamp: i64) -> Option<NaiveDateTime> {
    Some(
        DateTime::from_timestamp(timestamp, 0)?
            .with_timezone(&Local)
            .naive_local(),
    )
}

/// Add months to a date time; the day is clamped to the end of a shorter month (Jan 31 + 1 month
/// is Feb 28 or 29).
fn add_months(naive: NaiveDateTime, months: i64) -> Option<NaiveDateTime> {
    let delta = chrono::Months::new(u32::try_from(months.unsigned_abs()).ok()?);
    if months >= 0 {
        naive.checked_add_months(delta)
    } else {
        naive.checked_sub_months(delta)
    }
}

fn try_date_add(timestamp: i64, n: i64, unit: DateUnit) -> Option<i64> {
    if let Some(seconds) = unit.seconds() {
        return timestamp.checked_add(n.checked_mul(seconds)?);
    }
    let naive = local_naive(timestamp)?;
    let naive = match unit.months() {
        Some(months) => add_months(naive, n.checked_mul(months)?)?,
        None => {
            let days = if unit == DateUnit::Week {
                n.checked_mul(7)?
            } else {
                n
            };
            naive.checked_add_signed(chrono::TimeDelta::try_days(days)?)?
        }
    };
    local_timestamp(&Local, &naive)
}

/// `date_add(timestamp, n, unit)`: the timestamp `n` units later (earlier if `n` is negative), in
/// local time; `-1` with a warning for an unknown unit or a result out of range.
pub fn date_add(timestamp: Int, n: Int, unit: &str) -> Int {
    let Some(date_unit) = DateUnit::parse(unit) else {
        runtime::stdlib_warning("date_add", format!("invalid unit {:?}", unit));
        return -1;
    };
    try_date_add(timestamp, n, date_unit).unwrap_or_else(|| {
        runtime::stdlib_warning(
            "date_add",
            format!("date out of range: {} {} {}", timestamp, n, unit),
        );
        -1
    })
}

/// `date_add(text, n, unit)`: as [`date_add`], with the time given as text: a numeric string is a
/// Unix timestamp, other text is parsed as `mktime` does in local time; `-1` with a warning if it
/// cannot be parsed.
pub fn date_add_str(date_time_text: &str, n: Int, unit: &str) -> Int {
    match parse_time_text("date_add", date_time_text) {
        Some(timestamp) => date_add(timestamp, n, unit),
        None => -1,
    }
}

/// A time argument given as text: a numeric string is a Unix timestamp, other text is parsed as
/// `mktime` does in local time; `None` with a warning naming `func` if it can't be parsed.
fn parse_time_text(func: &str, date_time_text: &str) -> Option<Int> {
    match runtime::compare::looks_numeric(date_time_text.as_bytes()) {
        Some(num) => Some(num as Int),
        None => {
            let timestamp = mktime_tz(date_time_text, &Local);
            if timestamp.is_none() {
                runtime::stdlib_warning(func, format!("invalid date time {:?}", date_time_text));
            }
            timestamp
        }
    }
}

fn try_date_diff(unit: DateUnit, start: i64, end: i64) -> Option<i64> {
    if let Some(seconds) = unit.seconds() {
        return Some(end.checked_sub(start)? / seconds);
    }
    let (start, end) = (local_naive(start)?, local_naive(end)?);
    let Some(unit_months) = unit.months() else {
        let days = (end - start).num_days();
        return Some(if unit == DateUnit::Week {
            days / 7
        } else {
            days
        });
    };
    let mut months =
        (end.year() as i64 - start.year() as i64) * 12 + end.month() as i64 - start.month() as i64;
    // only complete months count, as MySQL: compare the (day, time) rest without clamping month
    // ends, so swapping `start` and `end` only negates the result
    let (start_rest, end_rest) = ((start.day(), start.time()), (end.day(), end.time()));
    if months > 0 && end_rest < start_rest {
        months -= 1;
    } else if months < 0 && end_rest > start_rest {
        months += 1;
    }
    Some(months / unit_months)
}

/// `date_diff(unit, start, end)`: the number of complete units from `start` to `end` (negative if
/// `end` is earlier), as MySQL's `TIMESTAMPDIFF`; `0` with a warning for an unknown unit.
pub fn date_diff(unit: &str, start: Int, end: Int) -> Int {
    let Some(date_unit) = DateUnit::parse(unit) else {
        runtime::stdlib_warning("date_diff", format!("invalid unit {:?}", unit));
        return 0;
    };
    try_date_diff(date_unit, start, end).unwrap_or_else(|| {
        runtime::stdlib_warning("date_diff", format!("date out of range: {} {}", start, end));
        0
    })
}

/// `date_diff(unit, start, end)` with the times given as text, parsed as [`date_add_str`] does;
/// `0` with a warning if either can't be parsed.
pub fn date_diff_str(unit: &str, start: &str, end: &str) -> Int {
    match (
        parse_time_text("date_diff", start),
        parse_time_text("date_diff", end),
    ) {
        (Some(start), Some(end)) => date_diff(unit, start, end),
        _ => 0,
    }
}

fn try_date_trunc(unit: DateUnit, timestamp: i64) -> Option<i64> {
    if let Some(seconds) = unit.seconds() {
        // truncate by the offset at this instant, so both fall back hours keep their own offset
        let offset = DateTime::from_timestamp(timestamp, 0)?
            .with_timezone(&Local)
            .offset()
            .fix()
            .local_minus_utc();
        let local = timestamp.checked_add(offset as i64)?;
        return timestamp.checked_sub(local.rem_euclid(seconds));
    }
    let date = local_naive(timestamp)?.date();
    let date = match unit {
        DateUnit::Day => date,
        // weeks start on Monday, as ISO 8601
        DateUnit::Week => {
            date - chrono::TimeDelta::days(date.weekday().num_days_from_monday() as i64)
        }
        _ => {
            let unit_months = unit.months()? as u32;
            let month0 = date.month0() / unit_months * unit_months;
            NaiveDate::from_ymd_opt(date.year(), month0 + 1, 1)?
        }
    };
    local_timestamp(&Local, &date.and_time(NaiveTime::MIN))
}

/// `date_trunc(unit, timestamp)`: the timestamp truncated to the start of its unit (hour, day,
/// Monday of the week, first day of the month/quarter/year) in local time; `-1` with a warning for
/// an unknown unit or a timestamp out of range.
pub fn date_trunc(unit: &str, timestamp: Int) -> Int {
    let Some(date_unit) = DateUnit::parse(unit) else {
        runtime::stdlib_warning("date_trunc", format!("invalid unit {:?}", unit));
        return -1;
    };
    try_date_trunc(date_unit, timestamp).unwrap_or_else(|| {
        runtime::stdlib_warning("date_trunc", format!("date out of range: {}", timestamp));
        -1
    })
}

/// `date_trunc(unit, time)` with the time given as text, parsed as [`date_add_str`] does; `-1`
/// with a warning if it can't be parsed.
pub fn date_trunc_str(unit: &str, date_time_text: &str) -> Int {
    match parse_time_text("date_trunc", date_time_text) {
        Some(timestamp) => date_trunc(unit, timestamp),
        None => -1,
    }
}

/// `to_iso8601(timestamp)`: the timestamp as an ISO 8601 UTC date time in the format of
/// JavaScript's `Date.prototype.toISOString`, e.g. `1970-01-01T00:00:00.000Z`; the timestamp is in
/// whole seconds, so the milliseconds are always `.000`. An empty string with a warning for a
/// timestamp out of range.
pub fn to_iso8601(timestamp: Int) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(date_time) => date_time.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        None => {
            runtime::stdlib_warning("to_iso8601", format!("date out of range: {}", timestamp));
            String::new()
        }
    }
}

/// `to_iso8601(time)` with the time given as text, parsed as [`date_add_str`] does; an empty
/// string with a warning if it can't be parsed.
pub fn to_iso8601_str(date_time_text: &str) -> String {
    match parse_time_text("to_iso8601", date_time_text) {
        Some(timestamp) => to_iso8601(timestamp),
        None => String::new(),
    }
}

/// `strptime(text, format[, timezone])`: parse `text` with the chrono strftime `format` (the
/// format `strftime` takes) to a Unix time, `-1` if the text does not match the format.
///
/// Unlike `mktime`, nothing is guessed: `"01/02/2024"` is January 2 with `"%m/%d/%Y"` and February
/// 1 with `"%d/%m/%Y"`. A format without a time of day means midnight. Text with a UTC offset
/// (`%z`) or a Unix time (`%s`) is an instant; any other text is a local time in `timezone`: the
/// local time zone when it is empty or `"0"`, the zone of the IANA database it names
/// (`"Asia/Shanghai"`), or UTC for any other value (a true utc flag, as in `strftime`).
///
/// A zone abbreviation (`%Z`) is not a zone: `CST` is China, US Central or Cuba. It is only taken
/// when it is `UTC`, `GMT`, `UT` or `Z`, or the abbreviation `timezone` uses at that time (`CST`
/// with `"Asia/Shanghai"`, `EST`/`EDT` with `"America/New_York"`); any other is `-1` with a
/// warning rather than a wrong instant.
pub fn strptime(text: &str, format: &str, timezone: &str) -> Int {
    if has_zone_name(format) {
        return strptime_zone_name(text, format, timezone).unwrap_or(-1);
    }
    try_strptime(text, format, timezone).unwrap_or(-1)
}

/// Whether the strftime `format` has a `%Z` (a zone abbreviation), not counting `%%Z`.
fn has_zone_name(format: &str) -> bool {
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('Z') => return true,
                Some(_) => {}
                None => break,
            }
        }
    }
    false
}

/// `format` with each `%Z` replaced by the literal `abbreviation` (which has no `%`).
fn replace_zone_name(format: &str, abbreviation: &str) -> String {
    let mut result = String::with_capacity(format.len() + abbreviation.len());
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('Z') => result.push_str(abbreviation),
            Some(next) => {
                result.push('%');
                result.push(next);
            }
            None => result.push('%'),
        }
    }
    result
}

/// The local date and time `text` gives in `format`, with no offset applied. A format without a
/// time of day means midnight; one with only an hour (`%H`, `%I%p`) means the start of that hour,
/// and the minute or second left out is 0. chrono alone wants at least the hour and the minute.
fn parse_naive(text: &str, format: &str) -> Option<NaiveDateTime> {
    use chrono::format::{Parsed, StrftimeItems, parse};
    let mut parsed = Parsed::new();
    parse(&mut parsed, text, StrftimeItems::new(format)).ok()?;
    let has_time = parsed.hour_div_12().is_some()
        || parsed.hour_mod_12().is_some()
        || parsed.minute().is_some()
        || parsed.second().is_some()
        || parsed.timestamp().is_some();
    if !has_time {
        return Some(parsed.to_naive_date().ok()?.and_time(NaiveTime::MIN));
    }
    if parsed.minute().is_none() {
        parsed.set_minute(0).ok()?;
    }
    parsed.to_naive_datetime_with_offset(0).ok()
}

/// `strptime` for a format with `%Z`: the abbreviation in the text must be a UTC name or one the
/// `timezone` zone uses at that local time, see [`strptime`].
fn strptime_zone_name(text: &str, format: &str, timezone: &str) -> Option<Int> {
    // a %z in the same text gives the instant already
    if let Ok(date_time) = DateTime::parse_from_str(text, format) {
        return Some(date_time.timestamp());
    }
    // chrono reads %Z as any word and drops it: find the time first, then which name it was
    let naive = parse_naive(text, format)?;
    let matches = |abbreviation: &str| {
        let literal = replace_zone_name(format, abbreviation);
        parse_naive(text, &literal).is_some()
    };
    if ["UTC", "GMT", "UT", "Z"].iter().any(|name| matches(name)) {
        return Some(naive.and_utc().timestamp());
    }
    let tz = match timezone {
        "" | "0" => jiff::tz::TimeZone::system(),
        name => jiff::tz::TimeZone::get(name).unwrap_or(jiff::tz::TimeZone::UTC),
    };
    let civil = jiff::civil::DateTime::new(
        i16::try_from(naive.year()).ok()?,
        naive.month() as i8,
        naive.day() as i8,
        naive.hour() as i8,
        naive.minute() as i8,
        naive.second() as i8,
        0,
    )
    .ok()?;
    // the offsets the local time can have (two in a fold or gap), each kept only when the zone
    // really has that offset then and calls it by the name in the text: EST or EDT in a fold
    let offsets = match tz.to_ambiguous_timestamp(civil).offset() {
        jiff::tz::AmbiguousOffset::Unambiguous { offset } => vec![offset],
        jiff::tz::AmbiguousOffset::Gap { before, after }
        | jiff::tz::AmbiguousOffset::Fold { before, after } => vec![before, after],
    };
    for offset in offsets {
        let Ok(timestamp) = offset.to_timestamp(civil) else {
            continue;
        };
        let info = tz.to_offset_info(timestamp);
        if info.offset() == offset && matches(info.abbreviation()) {
            return Some(timestamp.as_second());
        }
    }
    runtime::stdlib_warning(
        "strptime",
        format!(
            "zone name in {:?} is neither UTC nor used by time zone {:?} then; use %z or the right timezone",
            text,
            tz.iana_name().unwrap_or("local")
        ),
    );
    None
}

fn try_strptime(text: &str, format: &str, timezone: &str) -> Option<Int> {
    // the text gives the instant: an offset, or a Unix time with %s
    if let Ok(date_time) = DateTime::parse_from_str(text, format) {
        return Some(date_time.timestamp());
    }
    let naive = parse_naive(text, format)?;
    match timezone {
        "" | "0" => local_timestamp(&Local, &naive),
        name => match jiff::tz::TimeZone::get(name) {
            Ok(tz) => {
                let civil = jiff::civil::DateTime::new(
                    i16::try_from(naive.year()).ok()?,
                    naive.month() as i8,
                    naive.day() as i8,
                    naive.hour() as i8,
                    naive.minute() as i8,
                    naive.second() as i8,
                    0,
                )
                .ok()?;
                Some(civil.to_zoned(tz).ok()?.timestamp().as_second())
            }
            Err(_) => Some(naive.and_utc().timestamp()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> i64 {
        mktime(text, true, MKTIME_LOCAL_TIMEZONE)
    }

    #[test]
    fn test_strptime() {
        // day and month are where the format says
        assert_eq!(strptime("01/02/2024", "%m/%d/%Y", "UTC"), 1704153600);
        assert_eq!(strptime("01/02/2024", "%d/%m/%Y", "UTC"), 1706745600);
        assert_eq!(
            strptime("2024-01-02 03:04:05", "%Y-%m-%d %H:%M:%S", "1"),
            1704164645
        );
        // an IANA zone, or the offset in the text
        assert_eq!(
            strptime("2024-01-02 08:00", "%Y-%m-%d %H:%M", "Asia/Shanghai"),
            1704153600
        );
        assert_eq!(
            strptime("2024-01-02 08:00 +0800", "%Y-%m-%d %H:%M %z", ""),
            1704153600
        );
        assert_eq!(strptime("1704153600", "%s", ""), 1704153600);
        // only an hour: the start of that hour, not midnight
        assert_eq!(strptime("2024-01-02 15", "%Y-%m-%d %H", "UTC"), 1704207600);
        assert_eq!(strptime("2024-01-02 3PM", "%Y-%m-%d %I%p", "UTC"), 1704207600);
        assert_eq!(
            strptime("2024-01-02 15 UTC", "%Y-%m-%d %H %Z", ""),
            1704207600
        );
        // local time by default, as mktime
        assert_eq!(
            strptime("2024 01 02 03 04 05", "%Y %m %d %H %M %S", ""),
            mktime("2024 01 02 03 04 05", false, MKTIME_LOCAL_TIMEZONE)
        );
        // text that does not match the format
        assert_eq!(strptime("2024-13-01", "%Y-%m-%d", "UTC"), -1);
        assert_eq!(strptime("2024-01-02 x", "%Y-%m-%d", "UTC"), -1);
        assert_eq!(strptime("", "%Y-%m-%d", "UTC"), -1);
    }

    #[test]
    fn test_strptime_zone_name() {
        let format = "%Y-%m-%d %H:%M:%S %Z";
        // UTC names are UTC whatever the timezone
        assert_eq!(strptime("2024-01-02 08:00:00 UTC", format, "Asia/Shanghai"), 1704182400);
        assert_eq!(strptime("2024-01-02 08:00:00 GMT", format, ""), 1704182400);
        // the abbreviation of the given zone at that time
        assert_eq!(strptime("2024-01-02 08:00:00 CST", format, "Asia/Shanghai"), 1704153600);
        assert_eq!(strptime("2024-01-02 08:00:00 EST", format, "America/New_York"), 1704200400);
        assert_eq!(strptime("2024-07-02 08:00:00 EDT", format, "America/New_York"), 1719921600);
        // a fold: 01:30 happens twice, EDT then EST
        assert_eq!(strptime("2024-11-03 01:30:00 EDT", format, "America/New_York"), 1730611800);
        assert_eq!(strptime("2024-11-03 01:30:00 EST", format, "America/New_York"), 1730615400);
        // a name the zone does not use then, or no zone at all: not a guessed instant
        assert_eq!(strptime("2024-01-02 08:00:00 CST", format, "UTC"), -1);
        assert_eq!(strptime("2024-01-02 08:00:00 EST", format, "UTC"), -1);
        assert_eq!(strptime("2024-01-02 08:00:00 XYZ", format, "UTC"), -1);
        assert_eq!(strptime("2024-01-02 08:00:00 EDT", format, "America/New_York"), -1);
        // %%Z is a literal "%Z", not a zone name
        assert_eq!(strptime("2024-01-02 %Z", "%Y-%m-%d %%Z", "UTC"), 1704153600);
        assert!(!has_zone_name("%Y %%Z"));
    }

    #[test]
    fn test_date_add_diff_units() {
        // fixed-length units do not depend on the time zone
        let t = utc("2024 01 31 10 00 00");
        assert_eq!(try_date_add(t, 90, DateUnit::Minute), Some(t + 5400));
        assert_eq!(try_date_add(t, -2, DateUnit::Hour), Some(t - 7200));
        assert_eq!(try_date_diff(DateUnit::Hour, t, t + 7199), Some(1));
        assert_eq!(try_date_diff(DateUnit::Minute, t + 119, t), Some(-1));
        assert_eq!(DateUnit::parse(" Months "), Some(DateUnit::Month));
        assert_eq!(DateUnit::parse("fortnight"), None);
        assert_eq!(try_date_add(i64::MAX, 1, DateUnit::Second), None);
        assert_eq!(try_date_trunc(DateUnit::Second, t + 59), Some(t + 59));
    }

    #[test]
    fn test_date_diff_months_symmetric() {
        // month ends are not clamped: Jan 31 to Feb 28 and Feb 29 to the next Feb 28 are not complete
        for (unit, start, end, expected) in [
            (
                DateUnit::Month,
                "2023 01 31 00 00 00",
                "2023 02 28 00 00 00",
                0,
            ),
            (
                DateUnit::Month,
                "2023 01 31 00 00 00",
                "2023 03 31 00 00 00",
                2,
            ),
            (
                DateUnit::Month,
                "2023 01 31 10 00 00",
                "2023 03 31 09 59 59",
                1,
            ),
            (
                DateUnit::Year,
                "2020 02 29 00 00 00",
                "2021 02 28 00 00 00",
                0,
            ),
            (
                DateUnit::Year,
                "2020 02 29 00 00 00",
                "2021 03 01 00 00 00",
                1,
            ),
            (
                DateUnit::Quarter,
                "2023 11 30 00 00 00",
                "2024 02 29 00 00 00",
                0,
            ),
        ] {
            let local = |text| mktime(text, false, MKTIME_LOCAL_TIMEZONE);
            let (start, end) = (local(start), local(end));
            assert_eq!(
                try_date_diff(unit, start, end),
                Some(expected),
                "{unit:?} {start} {end}"
            );
            assert_eq!(
                try_date_diff(unit, end, start),
                Some(-expected),
                "{unit:?} {end} {start}"
            );
        }
    }

    #[test]
    fn test_strftime_invalid_format() {
        // an unknown conversion is output as is: `Q` (BSD) or `%Q` (glibc)
        let text = strftime_tz("%Y-%Q", 0, &Zone::Utc);
        assert!(text == "1970-Q" || text == "1970-%Q", "{text}");
        // out of range timestamp
        assert_eq!(strftime("%Y", 1 << 62), "");
    }

    #[test]
    fn test_strftime_named_zone() {
        let at = |name: &str, ts: i64| Zone::named(name, ts).unwrap();
        let ts = 1621530000; // 2021-05-20 17:00:00 UTC
        assert_eq!(
            strftime_tz("%Y-%m-%d %H:%M:%S %z", ts, &at("Asia/Shanghai", ts)),
            "2021-05-21 01:00:00 +0800"
        );
        // daylight saving time in summer, not in winter
        assert_eq!(
            strftime_tz("%H:%M %Z", ts, &at("America/New_York", ts)),
            "13:00 EDT"
        );
        let winter = 1610000000; // 2021-01-07 06:13:20 UTC
        assert_eq!(
            strftime_tz("%H:%M %Z", winter, &at("America/New_York", winter)),
            "01:13 EST"
        );
        assert_eq!(strftime_tz("%H:%M", ts, &at("UTC", ts)), "17:00");
        assert!(Zone::named("Not/AZone", ts).is_none());
        assert!(Zone::named("1", ts).is_none());
        assert!(Zone::named("", ts).is_none());
    }

    #[test]
    fn test_strftime_utc() {
        assert_eq!(
            strftime_tz("%Y-%m-%d %H:%M:%S", 1621530000, &Zone::Utc),
            "2021-05-20 17:00:00"
        );
        assert_eq!(strftime_tz("%Ey/%Od", 1621530000, &Zone::Utc), "21/20");
    }

    #[test]
    fn test_strftime() {
        let format = "%c";
        let timestamp = 1621530000;
        println!("{}", strftime(format, timestamp));
    }

    #[test]
    fn test_datetime_consistent_with_strftime() {
        let timestamp = 1621530000;
        let dt = datetime2(timestamp);
        let fields = [
            ("%Y", "year"),
            ("%m", "month"),
            ("%d", "monthday"),
            ("%H", "hour"),
            ("%M", "minute"),
            ("%S", "second"),
            ("%j", "yearday"),
            ("%V", "week"),
            ("%G", "isoyear"),
        ];
        for (format, key) in fields {
            assert_eq!(
                strftime(format, timestamp).parse::<Int>().unwrap(),
                dt.get(&Str::from(key)),
                "{}",
                key
            );
        }
        // date time text without offset is parsed as local time
        let datetime_info = datetime("2026-10-25 10:11:12");
        assert_eq!(datetime_info.get(&Str::from("hour")), 10);
        // Sunday should be 0
        assert_eq!(datetime_info.get(&Str::from("weekday")), 0);
        assert_eq!(datetime_info.get(&Str::from("quarter")), 4);
        // 2021-01-01 belongs to ISO week 53 of 2020
        let datetime_info = datetime("2021-01-01 12:00:00");
        assert_eq!(datetime_info.get(&Str::from("quarter")), 1);
        assert_eq!(datetime_info.get(&Str::from("week")), 53);
        assert_eq!(datetime_info.get(&Str::from("isoyear")), 2020);
        assert_eq!(datetime_info.get(&Str::from("year")), 2021);
        assert_eq!(datetime_info.get(&Str::from("month_lastday")), 31);
        // leap year February and month without 31 days
        assert_eq!(datetime("2024-02-10").get(&Str::from("month_lastday")), 29);
        assert_eq!(datetime("2023-02-10").get(&Str::from("month_lastday")), 28);
        assert_eq!(datetime("2023-04-30").get(&Str::from("month_lastday")), 30);
        assert_eq!(datetime("2023-12-05").get(&Str::from("month_lastday")), 31);
    }

    #[test]
    fn test_date_parse() {
        let date_text_items = vec![
            "Thursday, 20 May 2021",
            "2024-04-27 17:07:25.684184848 +08:00",
            "09:11:12 -1day",
        ];
        for item in date_text_items {
            assert_ne!(mktime(item, true, MKTIME_LOCAL_TIMEZONE), -1, "{}", item);
        }
    }

    #[test]
    fn test_mktime_timezone() {
        // 2024-01-01 10:00:00 at UTC+8 is 2024-01-01 02:00:00 UTC
        assert_eq!(mktime("2024-01-01 10:00:00", false, 8), 1704074400);
        // 2024-01-01 10:00:00 at UTC-5 is 2024-01-01 15:00:00 UTC
        assert_eq!(mktime("2024-01-01 10:00:00", false, -5), 1704121200);
        assert_eq!(mktime("2024-01-01 10:00:00", false, 0), 1704103200);
        // gawk format
        assert_eq!(mktime("2024 01 01 10 00 00", false, 8), 1704074400);
        assert_eq!(mktime("2024 01 01 10 00 00", false, -5), 1704121200);
        // explicit offset in text wins over timezone argument
        assert_eq!(mktime("2024-01-01 10:00:00 +08:00", false, -5), 1704074400);
        // before 1970
        assert_eq!(mktime("1969-12-31 23:00:00", false, 0), -3600);
    }

    #[test]
    fn test_mktime_gawk() {
        // utc flag, as gawk's mktime(spec, utc-flag)
        assert_eq!(
            mktime("1970 01 02 00 00 00", true, MKTIME_LOCAL_TIMEZONE),
            86400
        );
        assert_eq!(
            mktime("2024-01-01 10:00:00", true, MKTIME_LOCAL_TIMEZONE),
            1704103200
        );
        // the timezone wins over the utc flag
        assert_eq!(mktime("2024 01 01 10 00 00", true, 8), 1704074400);
        // optional DST field, unpadded and out-of-range values are normalized
        assert_eq!(
            mktime("1970 1 2 0 0 0 -1", true, MKTIME_LOCAL_TIMEZONE),
            86400
        );
        assert_eq!(
            mktime("1969 13 1 24 0 0", true, MKTIME_LOCAL_TIMEZONE),
            86400
        );
        assert_eq!(
            mktime("1970 01 01 00 00 -1", true, MKTIME_LOCAL_TIMEZONE),
            -1
        );
        // invalid text is -1
        assert_eq!(mktime("not a date", true, MKTIME_LOCAL_TIMEZONE), -1);
        assert_eq!(mktime("", false, MKTIME_LOCAL_TIMEZONE), -1);
    }

    #[test]
    fn test_fend_date() {
        let text = "Thursday, 20 May 2021";
        println!("{}", is_fend_date(text));
    }

    #[test]
    fn test_datetime() {
        let result = datetime("1575043680");
        println!("{:?}", result);
    }

    #[test]
    fn test_datetime_invalid_text_is_empty() {
        assert_eq!(datetime("not a date").len(), 0);
        assert!(datetime("2024-01-15").len() > 0);
    }

    #[test]
    fn test_mktime_iso_local() {
        assert_eq!(
            mktime("2021-05-01T01:17:02", true, MKTIME_LOCAL_TIMEZONE),
            1619831822
        );
        assert_eq!(
            mktime("2021-05-01T01:17", true, MKTIME_LOCAL_TIMEZONE),
            1619831820
        );
        assert_eq!(
            mktime("2021-05-01T01:17:02.500", true, MKTIME_LOCAL_TIMEZONE),
            1619831822
        );
        assert_eq!(
            mktime("2021-05-01T01:17:02", false, 8),
            1619831822 - 8 * 3600
        );
        assert_ne!(
            mktime("2021-05-01T01:17:02", false, MKTIME_LOCAL_TIMEZONE),
            -1
        );
        // space separated, same as `T`
        assert_eq!(
            mktime("2021-05-01 01:17:02", true, MKTIME_LOCAL_TIMEZONE),
            1619831822
        );
        assert_eq!(
            mktime("2021-05-01 01:17", true, MKTIME_LOCAL_TIMEZONE),
            1619831820
        );
        assert_eq!(
            mktime("2021-05-01 01:17:02", false, 8),
            1619831822 - 8 * 3600
        );
        // explicit offsets still win
        assert_eq!(mktime("2021-05-01T01:17:02Z", false, 8), 1619831822);
        assert_eq!(mktime("2021-05-01 01:17:02Z", false, 8), 1619831822);
        assert_eq!(
            mktime("2021-05-01T01:17:02+08:00", true, MKTIME_LOCAL_TIMEZONE),
            1619831822 - 8 * 3600
        );
    }

    #[test]
    fn test_mktime_date_only_is_midnight() {
        // 2024-01-15 00:00:00 UTC, independent of the time of day the test runs
        assert_eq!(
            mktime("2024/01/15", true, MKTIME_LOCAL_TIMEZONE),
            1705276800
        );
        assert_eq!(
            mktime("2024-01-15", true, MKTIME_LOCAL_TIMEZONE),
            1705276800
        );
        assert_eq!(
            mktime("Monday, 15 January 2024", true, MKTIME_LOCAL_TIMEZONE),
            1705276800
        );
        assert_eq!(mktime("2024/01/15", false, 8), 1705276800 - 8 * 3600);
        assert_eq!(
            datetime2(mktime("2024/01/15", false, MKTIME_LOCAL_TIMEZONE)).get(&Str::from("hour")),
            0
        );
    }

    #[test]
    fn test_duration() {
        let text = "2min + 12sec";
        println!("{}", duration(text));
        let text = "100ms";
        println!("{}", duration(text));
    }

    #[test]
    fn test_duration_short_units() {
        assert_eq!(duration("1h30m"), 5400);
        assert_eq!(duration("1h 30m"), 5400);
        assert_eq!(duration("1d2h3m4s"), 93784);
        assert_eq!(duration("90s"), 90);
        assert_eq!(duration("1500ms"), 2);
        assert_eq!(duration("2min + 12sec"), 132);
        assert_eq!(duration("2min 12sec"), 132);
        assert_eq!(duration("1.5h"), 5400);
        assert_eq!(duration("2 hours"), 7200);
        assert_eq!(duration("(1h + 30m) * 2"), 10800);
    }
}
