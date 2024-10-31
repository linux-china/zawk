use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use chrono::Utc;
use lazy_static::lazy_static;
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use postgres::{Client, NoTls};
use uuid::Uuid;

lazy_static! {
    static ref PG_POOLS: Arc<Mutex<HashMap<String, Client>>> = Arc::new(Mutex::new(HashMap::new()));
}

pub(crate) fn pg_query<'a>(db_url: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pools = PG_POOLS.lock().unwrap();
    let client = pools.entry(db_url.to_string()).or_insert_with(|| {
        if db_url.starts_with("postgres://") {
            Client::connect(&db_url.replace("postgres://", "postgresql://"), NoTls).unwrap()
        } else {
            Client::connect(db_url, NoTls).unwrap()
        }
    });
    let rows = client.query(sql, &[]).unwrap();
    let mut index = 1;
    for row in rows {
        let mut items: Vec<String> = vec![];
        for i in 0..row.len() {
            let text_value = reflective_get(&row, i);
            items.push(text_value);
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
        index += 1;
    }
    map
}

pub(crate) fn pg_execute(db_url: &str, sql: &str) -> Int {
    let mut pools = PG_POOLS.lock().unwrap();
    let client = pools.entry(db_url.to_string()).or_insert_with(|| {
        if db_url.starts_with("postgres://") {
            Client::connect(&db_url.replace("postgres://", "postgresql://"), NoTls).unwrap()
        } else {
            Client::connect(db_url, NoTls).unwrap()
        }
    });
    client.execute(sql, &[]).unwrap_or(0) as Int
}

fn reflective_get(row: &postgres::Row, index: usize) -> String {
    let column_type = row.columns().get(index).map(|c| c.type_().name()).unwrap();
    // see https://docs.rs/sqlx/0.8.2/sqlx/postgres/types/index.html
    let value = match column_type {
        "bool" => {
            let v: Option<bool> = row.get(index);
            v.map(|v| v.to_string())
        }
        "varchar" | "char(n)" | "text" | "name" => {
            let v: Option<String> = row.get(index);
            v
        }
        "char" => {
            let v: i8 = row.get(index);
            Some(String::from((v as u8) as char))
        }
        "int2" | "smallserial" | "smallint" => {
            let v: Option<i16> = row.get(index);
            v.map(|v| v.to_string())
        }
        "int" | "int4" | "serial" => {
            let v: Option<i32> = row.get(index);
            v.map(|v| v.to_string())
        }
        "int8" | "bigserial" | "bigint" => {
            let v: Option<i64> = row.get(index);
            v.map(|v| v.to_string())
        }
        "float4" | "real" => {
            let v: Option<f32> = row.get(index);
            v.map(|v| v.to_string())
        }
        "float8" | "double precision" => {
            let v: Option<f64> = row.get(index);
            v.map(|v| v.to_string())
        }
        "date" => {
            let v: Option<time::Date> = row.get(index);
            v.map(|v| v.to_string())
        }
        "time" => {
            let v: Option<time::Time> = row.get(index);
            v.map(|v| v.to_string())
        }
        "timestamp" | "timestamptz" => {
            // with-chrono feature is needed for this
            let v: Option<chrono::DateTime<Utc>> = row.get(index);
            v.map(|v| v.to_string())
        }
        "uuid" => {
            let v: Option<Uuid> = row.get(index);
            v.map(|v| v.to_string())
        }
        &_ => Some("".to_string()),
    };
    value.unwrap_or("".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike() {}

    #[test]
    fn test_query() {
        let sql = "SELECT name FROM city";
        let db_url = "postgres://postgres:postgres@localhost/demo";
        let rows = pg_query(db_url, sql);
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    fn test_delete_row() {
        let sql = "delete from blogs where id = 2";
        let db_url = "postgresql://postgres:postgres@localhost/demo";
        let _ = pg_execute(db_url, sql);
    }
}
