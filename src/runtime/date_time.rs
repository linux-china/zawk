use std::time::SystemTime;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use crate::runtime::date_time;

pub fn strftime(format: &str, timestamp: i64) -> String {
    let utc_now = NaiveDateTime::from_timestamp_opt(timestamp, 0).unwrap();
    let local_now: DateTime<Local> = Local.from_utc_datetime(&utc_now);
    local_now.format(&format.to_string()).to_string()
}

pub fn mktime(date_time_text: &str, timezone: i64) -> u64 {
    let dt_text_timezone = if timezone > 0 {
        format!("{} {}", date_time_text, timezone_offset_text(timezone))
    } else {
        date_time_text.to_string()
    };
    if let Ok(date_time) = dateparser::parse(&dt_text_timezone) {
        return date_time.timestamp() as u64;
    } else {
        let dt_text = format!("{} {}", date_time_text, timezone_offset_text(timezone));
        //gawk compatible parser
        if let Ok(date_time) = DateTime::parse_from_str(&dt_text, "%Y %m %d %H %M %S %z") {
            return date_time.timestamp() as u64;
        }
    }
    0
}

fn timezone_offset_text(timezone: i64) -> String {
    if timezone >= 10 {
        format!("+{}:00", timezone)
    } else if timezone >= 0 && timezone < 10 {
        format!("+0{}:00", timezone)
    } else {
        "+00:00".to_owned()
    }
}