use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use lazy_static::lazy_static;
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use mysql::*;
use mysql::prelude::*;

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
                Value::Date(year, month, day, hour, minutes, seconds, _micro_seconds) => {
                    format!("{}-{}-{} {}:{}:{}", year, month, day, hour, minutes, seconds)
                }
                Value::Time(_negative, _days, hours, minutes, seconds, _micro_seconds) => {
                    format!("{}:{}:{}", hours, minutes, seconds)
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
    fn test_delete_row() {
        let sql = "delete from people where id ='2'";
        let db_url = "mysql://root:123456@localhost:3306/test";
        let _ = mysql_execute(db_url, sql);
    }
}
