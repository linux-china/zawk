use crate::runtime;
use crate::runtime::{Int, Str};
use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Datelike, FixedOffset, Local, NaiveDateTime, TimeZone, Timelike};
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
pub const DEFAULT_STRFTIME_FORMAT: &str = "%a %m %e %H:%M:%S %Z %Y";

/// Format a unix timestamp as local date time, falling back to [`DEFAULT_STRFTIME_FORMAT`] for an invalid format.
pub fn strftime(format: &str, timestamp: i64) -> String {
    let format = if StrftimeItems::new(format).any(|item| item == Item::Error) {
        DEFAULT_STRFTIME_FORMAT
    } else {
        format
    };
    local_date_time(timestamp).format(format).to_string()
}

/// Sentinel timezone for `mktime(text)`: text without an explicit offset is treated as local time.
pub const MKTIME_LOCAL_TIMEZONE: i64 = i64::MIN;

/// Parse date time text to a unix timestamp.
/// `timezone` is the UTC offset in hours (e.g. `8` for UTC+8, `-5` for UTC-5) applied to text without an explicit offset;
/// [`MKTIME_LOCAL_TIMEZONE`] or an out-of-range value means local time.
pub fn mktime(date_time_text: &str, timezone: i64) -> i64 {
    let offset = timezone
        .checked_mul(3600)
        .and_then(|seconds| i32::try_from(seconds).ok())
        .and_then(FixedOffset::east_opt);
    match offset {
        Some(offset) => mktime_tz(date_time_text, &offset),
        None => mktime_tz(date_time_text, &Local),
    }
    .unwrap_or(0)
}

fn mktime_tz<Tz: TimeZone>(date_time_text: &str, tz: &Tz) -> Option<i64> {
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
    if is_fend_date(date_time_text) {
        let adjusted_dt_text = &date_time_text[date_time_text.find(' ').unwrap() + 1..];
        if let Ok(date_time) = dateparser::parse_with_timezone(adjusted_dt_text, tz) {
            return Some(date_time.timestamp());
        }
    }
    //gawk compatible parser
    if let Ok(naive) = NaiveDateTime::parse_from_str(date_time_text, "%Y %m %d %H %M %S") {
        return tz
            .from_local_datetime(&naive)
            .earliest()
            .map(|dt| dt.timestamp());
    }
    None
}

fn is_fend_date(text: &str) -> bool {
    if text.contains(',') {
        let temp = &text[0..text.find(',').unwrap()];
        return WEEKS.contains(&temp);
    }
    false
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
        let timestamp = mktime(date_time_text, MKTIME_LOCAL_TIMEZONE);
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
            let duration_ms = if result.contains(' ') {
                result[0..result.find(' ').unwrap()].parse::<Int>().unwrap()
            } else {
                result.parse::<Int>().unwrap()
            };
            if duration_ms % 1000 == 0 {
                duration_ms / 1000
            } else {
                (duration_ms as f64 / 1000.0).round() as Int
            }
        }
        Err(_) => 0,
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
            println!("{}", mktime(item, 0));
        }
    }

    #[test]
    fn test_mktime_timezone() {
        // 2024-01-01 10:00:00 at UTC+8 is 2024-01-01 02:00:00 UTC
        assert_eq!(mktime("2024-01-01 10:00:00", 8), 1704074400);
        // 2024-01-01 10:00:00 at UTC-5 is 2024-01-01 15:00:00 UTC
        assert_eq!(mktime("2024-01-01 10:00:00", -5), 1704121200);
        assert_eq!(mktime("2024-01-01 10:00:00", 0), 1704103200);
        // gawk format
        assert_eq!(mktime("2024 01 01 10 00 00", 8), 1704074400);
        assert_eq!(mktime("2024 01 01 10 00 00", -5), 1704121200);
        // explicit offset in text wins over timezone argument
        assert_eq!(mktime("2024-01-01 10:00:00 +08:00", -5), 1704074400);
        // before 1970
        assert_eq!(mktime("1969-12-31 23:00:00", 0), -3600);
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
