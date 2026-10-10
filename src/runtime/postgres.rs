use std::collections::HashMap;
use std::error::Error;
use std::fmt::Write;
use std::sync::{Arc, Mutex};
use chrono::Utc;
use lazy_static::lazy_static;
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::csv::vec_to_csv;
use postgres::types::{FromSql, Kind, Type};
use postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;
use uuid::Uuid;

lazy_static! {
    static ref PG_POOLS: Arc<Mutex<HashMap<String, Client>>> = Arc::new(Mutex::new(HashMap::new()));
}

/// Connect with TLS support, following libpq `sslmode` semantics:
/// `prefer`(default)/`require` encrypt without verifying the server certificate,
/// `verify-ca`/`verify-full` also verify the certificate, `disable` uses plain TCP.
fn connect(db_url: &str) -> Result<Client, String> {
    let mut conn_str = if db_url.starts_with("postgres://") {
        db_url.replacen("postgres://", "postgresql://", 1)
    } else {
        db_url.to_string()
    };
    // tokio-postgres only understands disable/prefer/require
    let mut verify_cert = false;
    for mode in ["sslmode=verify-full", "sslmode=verify-ca"] {
        if conn_str.contains(mode) {
            conn_str = conn_str.replace(mode, "sslmode=require");
            verify_cert = true;
        }
    }
    let tls_config = if verify_cert {
        crate::runtime::tls::client_config()
    } else {
        crate::runtime::tls::client_config_no_verify()
    };
    Client::connect(&conn_str, MakeRustlsConnect::new(tls_config?)).map_err(|e| e.to_string())
}

/// The cached client for `db_url`, connecting on first use.
fn client<'p>(pools: &'p mut HashMap<String, Client>, db_url: &str) -> Result<&'p mut Client, String> {
    if !pools.contains_key(db_url) {
        pools.insert(db_url.to_string(), connect(db_url)?);
    }
    Ok(pools.get_mut(db_url).unwrap())
}

pub(crate) fn pg_query<'a>(db_url: &str, sql: &str) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let mut pools = PG_POOLS.lock().unwrap_or_else(|e| e.into_inner());
    let rows = match client(&mut pools, db_url).and_then(|client| client.query(sql, &[]).map_err(|e| e.to_string())) {
        Ok(rows) => rows,
        Err(e) => {
            stdlib_warning("pg_query", e);
            return map;
        }
    };
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
    let mut pools = PG_POOLS.lock().unwrap_or_else(|e| e.into_inner());
    match client(&mut pools, db_url).and_then(|client| client.execute(sql, &[]).map_err(|e| e.to_string())) {
        Ok(n) => n as Int,
        Err(e) => {
            stdlib_warning("pg_execute", e);
            0
        }
    }
}

/// Text representation of a `numeric` value decoded from the binary wire format.
struct PgNumeric(String);

impl<'a> FromSql<'a> for PgNumeric {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        decode_numeric(raw).map(PgNumeric).ok_or_else(|| "invalid numeric value".into())
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

/// Enum value, sent as its label text on the wire.
struct PgEnum(String);

impl<'a> FromSql<'a> for PgEnum {
    fn from_sql(_: &Type, raw: &'a [u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(PgEnum(std::str::from_utf8(raw)?.to_string()))
    }

    fn accepts(ty: &Type) -> bool {
        matches!(ty.kind(), Kind::Enum(_))
    }
}

fn decode_numeric(raw: &[u8]) -> Option<String> {
    let read_u16 = |pos: usize| raw.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let ndigits = read_u16(0)? as usize;
    let weight = read_u16(2)? as i16 as i32;
    let sign = read_u16(4)?;
    let dscale = read_u16(6)? as usize;
    match sign {
        0xC000 => return Some("NaN".to_string()),
        0xD000 => return Some("Infinity".to_string()),
        0xF000 => return Some("-Infinity".to_string()),
        _ => {}
    }
    let digits = (0..ndigits).map(|i| read_u16(8 + i * 2)).collect::<Option<Vec<u16>>>()?;
    let digit = |i: i32| if i >= 0 { digits.get(i as usize).copied().unwrap_or(0) } else { 0 };
    let mut text = String::new();
    if sign == 0x4000 {
        text.push('-');
    }
    if weight < 0 {
        text.push('0');
    } else {
        for i in 0..=weight {
            if i == 0 {
                write!(text, "{}", digit(i)).ok()?;
            } else {
                write!(text, "{:04}", digit(i)).ok()?;
            }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut i = weight + 1;
        while frac.len() < dscale {
            write!(frac, "{:04}", digit(i)).ok()?;
            i += 1;
        }
        frac.truncate(dscale);
        text.push('.');
        text.push_str(&frac);
    }
    Some(text)
}

fn reflective_get(row: &postgres::Row, index: usize) -> String {
    let column_type = row.columns().get(index).map(|c| c.type_().name()).unwrap();
    // see https://docs.rs/sqlx/0.8.2/sqlx/postgres/types/index.html
    let value = match column_type {
        "bool" => {
            let v: Option<bool> = row.get(index);
            v.map(|v| v.to_string())
        }
        "varchar" | "bpchar" | "text" | "name" => {
            let v: Option<String> = row.get(index);
            v
        }
        "char" => {
            let v: Option<i8> = row.get(index);
            v.map(|v| String::from((v as u8) as char))
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
        "timestamp" => {
            // with-chrono feature is needed for this
            let v: Option<chrono::NaiveDateTime> = row.get(index);
            v.map(|v| v.to_string())
        }
        "timestamptz" => {
            let v: Option<chrono::DateTime<Utc>> = row.get(index);
            v.map(|v| v.to_string())
        }
        "numeric" => {
            let v: Option<PgNumeric> = row.get(index);
            v.map(|v| v.0)
        }
        "json" | "jsonb" => {
            // with-serde_json feature is needed for this
            let v: Option<serde_json::Value> = row.get(index);
            v.map(|v| v.to_string())
        }
        "bytea" => {
            let v: Option<Vec<u8>> = row.get(index);
            v.map(|v| format!("\\x{}", hex::encode(v)))
        }
        "oid" => {
            let v: Option<u32> = row.get(index);
            v.map(|v| v.to_string())
        }
        "inet" => {
            let v: Option<std::net::IpAddr> = row.get(index);
            v.map(|v| v.to_string())
        }
        "uuid" => {
            let v: Option<Uuid> = row.get(index);
            v.map(|v| v.to_string())
        }
        // enums and text-compatible types (citext etc.), otherwise empty
        &_ => match row.columns()[index].type_().kind() {
            Kind::Enum(_) => row.try_get::<_, Option<PgEnum>>(index).ok().flatten().map(|v| v.0),
            _ => row.try_get::<_, Option<String>>(index).ok().flatten(),
        },
    };
    value.unwrap_or("".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spike() {}

    fn numeric_raw(weight: i16, sign: u16, dscale: u16, digits: &[u16]) -> Vec<u8> {
        let mut raw = vec![];
        raw.extend((digits.len() as u16).to_be_bytes());
        raw.extend(weight.to_be_bytes());
        raw.extend(sign.to_be_bytes());
        raw.extend(dscale.to_be_bytes());
        for d in digits {
            raw.extend(d.to_be_bytes());
        }
        raw
    }

    #[test]
    fn test_decode_numeric() {
        // 12345.678
        assert_eq!(decode_numeric(&numeric_raw(1, 0, 3, &[1, 2345, 6780])).unwrap(), "12345.678");
        // -0.00012
        assert_eq!(decode_numeric(&numeric_raw(-2, 0, 8, &[1200])).unwrap(), "0.00001200");
        assert_eq!(decode_numeric(&numeric_raw(-1, 0x4000, 5, &[1, 2000])).unwrap(), "-0.00012");
        // 100000000 (trailing zero groups stripped)
        assert_eq!(decode_numeric(&numeric_raw(2, 0, 0, &[1])).unwrap(), "100000000");
        // 0.00
        assert_eq!(decode_numeric(&numeric_raw(0, 0, 2, &[])).unwrap(), "0.00");
        assert_eq!(decode_numeric(&numeric_raw(0, 0xC000, 0, &[])).unwrap(), "NaN");
    }

    #[test]
    #[ignore]
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
    #[ignore]
    fn test_delete_row() {
        let sql = "delete from blogs where id = 2";
        let db_url = "postgresql://postgres:postgres@localhost/demo";
        let _ = pg_execute(db_url, sql);
    }
}
