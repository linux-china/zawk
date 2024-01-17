use std::time::SystemTime;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};

pub fn strftime(format: &str, timestamp: i64) -> String {
    let utc_now = NaiveDateTime::from_timestamp_opt(timestamp, 0).unwrap();
    let local_now: DateTime<Local> = Local.from_utc_datetime(&utc_now);
    local_now.format(&format.to_string()).to_string()
}

pub fn mktime(date_time_text: &str) -> u64 {
    //todo date parse
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
}