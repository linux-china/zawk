use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use mysql::*;
use mysql::prelude::*;
use mysql::consts::ColumnType;

lazy_static! {
    static ref MYSQL_POOLS: Arc<Mutex<HashMap<String, Pool>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub(crate) fn mysql_query<'a>(db_url: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pools = MYSQL_POOLS.lock().unwrap();
    let pool = pools.entry(db_url.to_string()).or_insert_with(|| {
        Pool::new(db_url).unwrap()
    });
    let mut conn = pool.get_conn().unwrap();
    let rows: Vec<Row> = conn.query(sql).unwrap();
    let mut index = 1;
    for row in rows {
        let mut items: Vec<String> = vec![];
        for i in 0..row.len() {
            let col_value: Value = row.get(i).unwrap();
            let text_value = match col_value {
                Value::NULL => { "".to_owned() }
                Value::Bytes(bytes) => { String::from_utf8(bytes).unwrap_or("".to_owned()) }
                Value::Int(num) => { num.to_string() }
                Value::UInt(num) => { num.to_string() }
                Value::Float(num) => { num.to_string() }
                Value::Double(num) => { num.to_string() }
                Value::Date(year, month, day, hour, minutes, seconds, micro_seconds) => {
                    let date_only = row.columns_ref()[i].column_type() == ColumnType::MYSQL_TYPE_DATE;
                    format_mysql_date(year, month, day, hour, minutes, seconds, micro_seconds, date_only)
                }
                Value::Time(negative, days, hours, minutes, seconds, micro_seconds) => {
                    format_mysql_time(negative, days, hours, minutes, seconds, micro_seconds)
                }
            };
            items.push(text_value);
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
        index += 1;
    }
    map
}

fn format_mysql_date(year: u16, month: u8, day: u8, hour: u8, minutes: u8, seconds: u8, micro_seconds: u32, date_only: bool) -> String {
    if date_only {
        return format!("{:04}-{:02}-{:02}", year, month, day);
    }
    let mut text = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", year, month, day, hour, minutes, seconds);
    if micro_seconds > 0 {
        text.push_str(&format!(".{:06}", micro_seconds));
    }
    text
}

fn format_mysql_time(negative: bool, days: u32, hours: u8, minutes: u8, seconds: u8, micro_seconds: u32) -> String {
    let total_hours = days as u64 * 24 + hours as u64;
    let sign = if negative { "-" } else { "" };
    let mut text = format!("{}{:02}:{:02}:{:02}", sign, total_hours, minutes, seconds);
    if micro_seconds > 0 {
        text.push_str(&format!(".{:06}", micro_seconds));
    }
    text
}

pub(crate) fn mysql_execute(db_url: &str, sql: &str) -> Int {
    let mut pools = MYSQL_POOLS.lock().unwrap();
    let pool = pools.entry(db_url.to_string()).or_insert_with(|| {
        Pool::new(db_url).unwrap()
    });
    let mut conn = pool.get_conn().unwrap();
    let result: Vec<Row> = conn.exec(sql, Params::Empty).unwrap();
    result.len() as Int
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike() {}

    #[test]
    fn test_format_mysql_date() {
        assert_eq!(format_mysql_date(2024, 1, 5, 3, 4, 5, 0, false), "2024-01-05 03:04:05");
        assert_eq!(format_mysql_date(2024, 1, 5, 3, 4, 5, 123, false), "2024-01-05 03:04:05.000123");
        assert_eq!(format_mysql_date(2024, 1, 5, 0, 0, 0, 0, true), "2024-01-05");
    }

    #[test]
    fn test_format_mysql_time() {
        assert_eq!(format_mysql_time(false, 0, 3, 4, 5, 0), "03:04:05");
        assert_eq!(format_mysql_time(true, 1, 2, 3, 4, 0), "-26:03:04");
        assert_eq!(format_mysql_time(false, 34, 22, 59, 59, 500000), "838:59:59.500000");
    }

    #[test]
    #[ignore]
    fn test_query() {
        let sql = "SELECT id, name FROM people";
        let db_url = "mysql://root:123456@localhost:3306/test";
        let rows = mysql_query(db_url, sql);
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    #[ignore]
    fn test_delete_row() {
        let sql = "delete from people where id ='2'";
        let db_url = "mysql://root:123456@localhost:3306/test";
        let _ = mysql_execute(db_url, sql);
    }
}
