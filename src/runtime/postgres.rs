use std::any::Any;
use std::collections::HashMap;
use std::sync::Mutex;
use chrono::Utc;
use lazy_static::lazy_static;
use crate::runtime::{Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use uuid::Uuid;
use sqlx::{Column, Executor, PgPool, Row, TypeInfo};
use sqlx::postgres::{PgPoolOptions, PgRow};

lazy_static! {
    static ref PG_POOLS: Mutex<HashMap<String, PgPool>> = Mutex::new(HashMap::new());
}

pub(crate) async fn pg_query<'a>(db_url: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pools = PG_POOLS.lock().unwrap();
    if !pools.contains_key(db_url) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(db_url).await.unwrap();
        pools.insert(db_url.to_string(), pool);
    }
    let pool = pools.get(db_url).unwrap();
    let rows =sqlx::query(sql).fetch_all(pool).await.unwrap();
    let mut index = 1;
    for row in rows {
        let mut items: Vec<String> = vec![];
        for i in 0..row.columns().len() {
            let text_value = reflective_get(&row, i);
            items.push(text_value);
        }
        let v2: Vec<&str> = items.iter().map(|s| s as &str).collect();
        map.insert(index, Str::from(vec_to_csv(&v2)));
        index += 1;
    }
    map
}

pub(crate) async fn pg_execute(db_url: &str, sql: &str) -> Int {
    let mut pools = PG_POOLS.lock().unwrap();
    if !pools.contains_key(db_url) {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(db_url).await.unwrap();
        pools.insert(db_url.to_string(), pool);
    }
    let pool = pools.get(db_url).unwrap();
    let result = sqlx::query(sql).execute(&*pool).await.unwrap();
    result.rows_affected() as Int

}

fn reflective_get(row: &PgRow, index: usize) -> String {
    let column_type = row.columns().get(index).map(|c| c.type_info().name()).unwrap();
    // see https://docs.rs/sqlx/0.8.2/sqlx/postgres/types/index.html
    println!("column_type: {}", column_type);
    let value = match column_type {
        "BOOL" => {
            let v: Option<bool> = row.get(index);
            v.map(|v| v.to_string())
        }
        "VARCHAR" | "CHAR(N)" | "TEXT" | "NAME" | "CITEXT" => {
            let v: Option<String> = row.get(index);
            v
        }
        "CHAR" => {
            let v: i8 = row.get(index);
            Some(String::from((v as u8) as char))
        }
        "INT2" | "SMALLSERIAL" | "SMALLINT" => {
            let v: Option<i16> = row.get(index);
            v.map(|v| v.to_string())
        }
        "INT" | "INT4" | "SERIAL" => {
            let v: Option<i32> = row.get(index);
            v.map(|v| v.to_string())
        }
        "INT8" | "BIGSERIAL" | "BIGINT" => {
            let v: Option<i64> = row.get(index);
            v.map(|v| v.to_string())
        }
        "FLOAT4" | "REAL" => {
            let v: Option<f32> = row.get(index);
            v.map(|v| v.to_string())
        }
        "FLOAT8" | "DOUBLE PRECISION" => {
            let v: Option<f64> = row.get(index);
            v.map(|v| v.to_string())
        }
        "DATE" => {
            let v: Option<time::Date> = row.get(index);
            v.map(|v| v.to_string())
        }
        "TIME" => {
            let v: Option<time::Time> = row.get(index);
            v.map(|v| v.to_string())
        }
        "TIMESTAMP" | "TIMESTAMPTZ" => {
            // with-chrono feature is needed for this
            let v: Option<chrono::DateTime<Utc>> = row.get(index);
            v.map(|v| v.to_string())
        }
        "UUID" => {
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

    #[tokio::test]
    async fn test_query() {
        let sql = "SELECT name FROM city";
        let db_url = "postgres://postgres:postgres@localhost/demo";
        let rows = pg_query(db_url, sql).await;
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
