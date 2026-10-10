use crate::runtime;
use crate::runtime::{Int, Str, StrMap};
use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use std::time::SystemTime;

const WEEKS: [&'static str; 7] = [
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

/// The timestamp of `strftime(format)`: the current time. (Negative timestamps are dates before
/// 1970, so they cannot mark it.)
pub const STRFTIME_NOW: Int = Int::MIN;
/// Flags of [`awk_strftime`]: format in UTC rather than local time (the third argument of
/// `strftime`), and use `PROCINFO["strftime"]` as the format (`strftime()` without arguments).
pub const STRFTIME_UTC: Int = 1;
pub const STRFTIME_PROCINFO_FORMAT: Int = 2;

/// Format a unix timestamp as local date time, falling back to [`DEFAULT_STRFTIME_FORMAT`] for an invalid format.
pub fn strftime(format: &str, timestamp: i64) -> String {
    strftime_tz(format, timestamp, false)
}

fn strftime_tz(format: &str, timestamp: i64, utc: bool) -> String {
    let format = if StrftimeItems::new(format).any(|item| item == Item::Error) {
        DEFAULT_STRFTIME_FORMAT
    } else {
        format
    };
    if utc {
        let date_time: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
        date_time.format(format).to_string()
    } else {
        local_date_time(timestamp).format(format).to_string()
    }
}

/// awk's `strftime(format, timestamp, utc)`, with the arguments filled in by the compiler: the
/// timestamp is [`STRFTIME_NOW`] when it is not given, and `flags` holds [`STRFTIME_UTC`] and
/// [`STRFTIME_PROCINFO_FORMAT`]. An empty format gives an empty string, as in gawk.
pub(crate) fn awk_strftime(procinfo: &StrMap<Str>, format: &Str, timestamp: Int, flags: Int) -> String {
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
    let timestamp = if timestamp == STRFTIME_NOW {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    } else {
        timestamp
    };
    strftime_tz(&format, timestamp, flags & STRFTIME_UTC != 0)
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
    if let Some(naive) = parse_gawk_date_time(date_time_text) {
        return tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| dt.timestamp());
    }
    if let Some(timestamp) = chrono_systemd_time::parse_timestamp_tz(date_time_text, tz.clone())
        .ok()
        .and_then(|x| x.single())
    {
        return Some(timestamp.timestamp());
    }
    if let Ok(date_time) = dateparser::parse_with_timezone(date_time_text, tz) {
        return Some(date_time.timestamp());
    }
    // fend date format: Thursday, 20 May 2021
    if let Some(space) = date_time_text.find(' ').filter(|_| is_fend_date(date_time_text)) {
        let adjusted_dt_text = &date_time_text[space + 1..];
        if let Ok(date_time) = dateparser::parse_with_timezone(adjusted_dt_text, tz) {
            return Some(date_time.timestamp());
        }
    }
    None
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
        6 | 7 => [fields[0], fields[1], fields[2], fields[3], fields[4], fields[5]],
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
        return datetime2(seconds);
    } else if let Ok(timestamp) = date_time_text.parse::<i64>() {
        datetime2(timestamp)
    } else {
        let timestamp = mktime(date_time_text, false, MKTIME_LOCAL_TIMEZONE);
        datetime2(timestamp)
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
    result.insert(Str::from("weekday"), local_now.weekday() as Int);
    result.insert(Str::from("yearday"), local_now.ordinal() as Int);
    result
}

pub fn duration(text: &str) -> Int {
    let expr = format!("({}) to ms", text);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strftime_invalid_format() {
        assert_eq!(strftime("%Q", 0), strftime(DEFAULT_STRFTIME_FORMAT, 0));
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
        assert_eq!(datetime("2021-05-20 10:11:12").get(&Str::from("hour")), 10);
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
        assert_eq!(mktime("1970 01 02 00 00 00", true, MKTIME_LOCAL_TIMEZONE), 86400);
        assert_eq!(mktime("2024-01-01 10:00:00", true, MKTIME_LOCAL_TIMEZONE), 1704103200);
        // the timezone wins over the utc flag
        assert_eq!(mktime("2024 01 01 10 00 00", true, 8), 1704074400);
        // optional DST field, unpadded and out-of-range values are normalized
        assert_eq!(mktime("1970 1 2 0 0 0 -1", true, MKTIME_LOCAL_TIMEZONE), 86400);
        assert_eq!(mktime("1969 13 1 24 0 0", true, MKTIME_LOCAL_TIMEZONE), 86400);
        assert_eq!(mktime("1970 01 01 00 00 -1", true, MKTIME_LOCAL_TIMEZONE), -1);
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
    fn test_duration() {
        let text = "2min + 12sec";
        println!("{}", duration(text));
        let text = "100ms";
        println!("{}", duration(text));
    }
}
