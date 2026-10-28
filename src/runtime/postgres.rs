use std::error::Error;
use std::fmt::Write;
use std::sync::LazyLock;
use chrono::Utc;
use crate::runtime::{stdlib_warning, Int, IntMap, Str};
use crate::runtime::conn_pool::ConnPool;
use crate::runtime::csv::vec_to_csv;
use crate::runtime::numfmt::{f32_to_text, float_to_text};
use bytes::BytesMut;
use postgres::types::{to_sql_checked, FromSql, IsNull, Kind, ToSql, Type};
use postgres::Client;
use tokio_postgres_rustls::MakeRustlsConnect;
use uuid::Uuid;

static PG_POOLS: LazyLock<ConnPool<Client>> = LazyLock::new(ConnPool::new);

/// Connect with TLS support, following libpq `sslmode` semantics:
/// `prefer`(default)/`require` encrypt without verifying the server certificate,
/// `verify-ca`/`verify-full` also verify the certificate, `disable` uses plain TCP.
/// Connecting times out after 30s unless the URL sets `connect_timeout`.
pub(crate) fn connect(db_url: &str) -> Result<Client, String> {
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
    let mut config: postgres::Config = conn_str.parse().map_err(error_text)?;
    if config.get_connect_timeout().is_none() {
        config.connect_timeout(crate::runtime::network::CONNECT_TIMEOUT);
    }
    config.connect(MakeRustlsConnect::new(tls_config?)).map_err(error_text)
}

/// `postgres::Error` displays only its kind, such as "db error": append the cause.
pub(crate) fn error_text(e: postgres::Error) -> String {
    match e.source() {
        Some(cause) => format!("{}: {}", e, cause),
        None => e.to_string(),
    }
}

/// Run `f` with a cached client for `db_url`, connecting when none is idle. The client is taken
/// out of the cache while `f` runs, so no lock is held during network I/O. If a reused client
/// fails because its connection was closed (server restart, idle timeout), it is dropped so the
/// next call reconnects. With `retry`, `f` is also retried once with a new connection: only safe
/// for reads, since a write may have been committed before the connection was lost.
fn with_client<T>(db_url: &str, retry: bool, mut f: impl FnMut(&mut Client) -> Result<T, postgres::Error>) -> Result<T, String> {
    let (mut client, reused) = match PG_POOLS.take(db_url) {
        Some(client) => (client, true),
        None => (connect(db_url)?, false),
    };
    match f(&mut client) {
        Err(e) if client.is_closed() => {
            drop(client);
            if !(retry && reused) {
                return Err(format!("{} (not retried: the statement may already have been applied)", error_text(e)));
            }
            let mut client = connect(db_url).map_err(|reconnect| format!("{} (reconnect failed: {})", error_text(e), reconnect))?;
            let result = f(&mut client).map_err(error_text);
            if !client.is_closed() {
                PG_POOLS.put(db_url, client);
            }
            result
        }
        result => {
            PG_POOLS.put(db_url, client);
            result.map_err(error_text)
        }
    }
}

/// A positional parameter given as text, converted to the type the server inferred for it.
/// `position` is its 1-based `$n` placeholder number.
#[derive(Debug)]
struct PgParam {
    position: usize,
    text: String,
}

impl ToSql for PgParam {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        // the driver reports a 0-based index without the value: name the `$n`, value and type
        self.encode(ty, out)
            .map_err(|e| format!("${}: {:?} is not a valid {}: {}", self.position, self.text, ty, e).into())
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

impl PgParam {
    fn encode(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        let text = self.text.as_str();
        if <&str as ToSql>::accepts(ty) || matches!(ty.kind(), Kind::Enum(_)) {
            // text-like types and enum labels are sent as is
            return text.to_sql(ty, out);
        }
        // an empty (uninitialized) value is NULL for non-text types
        if text.is_empty() {
            return Ok(IsNull::Yes);
        }
        let value = text.trim();
        match *ty {
            Type::BOOL => match value.to_ascii_lowercase().as_str() {
                "t" | "true" | "y" | "yes" | "on" | "1" => true.to_sql(ty, out),
                "f" | "false" | "n" | "no" | "off" | "0" => false.to_sql(ty, out),
                _ => Err(format!("invalid boolean: {}", text).into()),
            },
            Type::CHAR => (value.bytes().next().unwrap_or(0) as i8).to_sql(ty, out),
            Type::INT2 => i16::try_from(parse_integer(value)?)?.to_sql(ty, out),
            Type::INT4 => i32::try_from(parse_integer(value)?)?.to_sql(ty, out),
            Type::INT8 => parse_integer(value)?.to_sql(ty, out),
            Type::OID => u32::try_from(parse_integer(value)?)?.to_sql(ty, out),
            Type::FLOAT4 => value.parse::<f32>()?.to_sql(ty, out),
            Type::FLOAT8 => value.parse::<f64>()?.to_sql(ty, out),
            Type::NUMERIC => match encode_numeric(value) {
                Some(raw) => {
                    out.extend_from_slice(&raw);
                    Ok(IsNull::No)
                }
                None => Err(format!("invalid numeric: {}", text).into()),
            },
            Type::JSON | Type::JSONB => serde_json::from_str::<serde_json::Value>(text)?.to_sql(ty, out),
            Type::UUID => Uuid::parse_str(value)?.to_sql(ty, out),
            Type::INET => value.parse::<std::net::IpAddr>()?.to_sql(ty, out),
            // `\\x...` is hex, as in the text format of bytea
            Type::BYTEA => match value.strip_prefix("\\x") {
                Some(hex_text) => hex::decode(hex_text)?.to_sql(ty, out),
                None => text.as_bytes().to_sql(ty, out),
            },
            Type::DATE => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")?.to_sql(ty, out),
            Type::TIME => parse_time(value)?.to_sql(ty, out),
            Type::TIMESTAMP => parse_timestamp(value)?.to_sql(ty, out),
            // without an offset the timestamp is taken as UTC
            Type::TIMESTAMPTZ => match chrono::DateTime::parse_from_rfc3339(value)
                .or_else(|_| chrono::DateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S%.f%#z"))
            {
                Ok(dt) => dt.with_timezone(&Utc).to_sql(ty, out),
                Err(_) => parse_timestamp(value)?.and_utc().to_sql(ty, out),
            },
            _ => Err(format!("unsupported parameter type, cast it in SQL, e.g. `${}::text::{}`", self.position, ty).into()),
        }
    }
}

/// Parse an integer, also accepting the integral float forms awk produces, such as `5.0` or `1e3`.
fn parse_integer(value: &str) -> Result<i64, Box<dyn Error + Sync + Send>> {
    if let Ok(n) = value.parse::<i64>() {
        return Ok(n);
    }
    let f = value.parse::<f64>().map_err(|_| "not a number")?;
    // 2^63 is exactly representable, so `f < 2^63` keeps the cast in range
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f < -(i64::MIN as f64) {
        Ok(f as i64)
    } else {
        Err("not an integer".into())
    }
}

fn parse_time(text: &str) -> Result<chrono::NaiveTime, chrono::ParseError> {
    chrono::NaiveTime::parse_from_str(text, "%H:%M:%S%.f")
        .or_else(|_| chrono::NaiveTime::parse_from_str(text, "%H:%M"))
}

fn parse_timestamp(text: &str) -> Result<chrono::NaiveDateTime, chrono::ParseError> {
    chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S%.f")
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").map(|d| d.and_time(chrono::NaiveTime::MIN)))
}

fn positional_params(params: &IntMap<Str>) -> Vec<PgParam> {
    crate::runtime::sqlite::positional_params(params)
        .into_iter()
        .enumerate()
        .map(|(i, text)| PgParam { position: i + 1, text })
        .collect()
}

pub(crate) fn pg_query<'a>(db_url: &str, sql: &str, params: &IntMap<Str>) -> IntMap<Str<'a>> {
    let map: IntMap<Str> = IntMap::default();
    let params = positional_params(params);
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    let rows = match with_client(db_url, true, |client| client.query(sql, &params)) {
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

pub(crate) fn pg_execute(db_url: &str, sql: &str, params: &IntMap<Str>) -> Int {
    let params = positional_params(params);
    let params: Vec<&(dyn ToSql + Sync)> = params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
    match with_client(db_url, false, |client| client.execute(sql, &params)) {
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

/// Binary wire format of a `numeric` from decimal text such as `-12.50`: base 10000 digits with
/// the weight of the first one; the inverse of `decode_numeric`.
fn encode_numeric(text: &str) -> Option<Vec<u8>> {
    let (negative, body) = match text.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let header = |ndigits: usize, weight: i16, sign: u16, dscale: u16| {
        let mut raw = Vec::with_capacity(8 + ndigits * 2);
        raw.extend((ndigits as u16).to_be_bytes());
        raw.extend(weight.to_be_bytes());
        raw.extend(sign.to_be_bytes());
        raw.extend(dscale.to_be_bytes());
        raw
    };
    if body.eq_ignore_ascii_case("nan") {
        return Some(header(0, 0, 0xC000, 0));
    }
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    if (int_part.is_empty() && frac_part.is_empty())
        || !int_part.bytes().chain(frac_part.bytes()).all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let int_part = int_part.trim_start_matches('0');
    let dscale = u16::try_from(frac_part.len()).ok()?;
    // pad the integer part on the left and the fraction on the right to groups of 4 digits
    let int_pad = (4 - int_part.len() % 4) % 4;
    let frac_pad = (4 - frac_part.len() % 4) % 4;
    let text = format!("{}{}{}{}", "0".repeat(int_pad), int_part, frac_part, "0".repeat(frac_pad));
    let mut digits: Vec<u16> = text.as_bytes().chunks(4)
        .map(|group| group.iter().fold(0, |n, b| n * 10 + (b - b'0') as u16))
        .collect();
    let mut weight = ((int_pad + int_part.len()) / 4) as i64 - 1;
    let leading_zeros = digits.iter().take_while(|&&d| d == 0).count();
    digits.drain(..leading_zeros);
    weight -= leading_zeros as i64;
    while digits.last() == Some(&0) {
        digits.pop();
    }
    if digits.is_empty() {
        weight = 0;
    }
    let sign = if negative && !digits.is_empty() { 0x4000 } else { 0 };
    let mut raw = header(digits.len(), i16::try_from(weight).ok()?, sign, dscale);
    for d in digits {
        raw.extend(d.to_be_bytes());
    }
    Some(raw)
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

pub(crate) fn reflective_get(row: &postgres::Row, index: usize) -> String {
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
            v.map(|v| f32_to_text(v))
        }
        "float8" | "double precision" => {
            let v: Option<f64> = row.get(index);
            v.map(|v| float_to_text(v))
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
    fn test_encode_numeric() {
        for text in ["12345.678", "0.00012", "-0.00012", "100000000", "0.00", "3.14", "-12.50", "0", "7", "NaN"] {
            assert_eq!(decode_numeric(&encode_numeric(text).unwrap()).unwrap(), text);
        }
        assert_eq!(decode_numeric(&encode_numeric("+007.10").unwrap()).unwrap(), "7.10");
        assert_eq!(decode_numeric(&encode_numeric(".5").unwrap()).unwrap(), "0.5");
        assert_eq!(decode_numeric(&encode_numeric("-0").unwrap()).unwrap(), "0");
        for text in ["", ".", "abc", "1e5", "1.2.3", "--1"] {
            assert!(encode_numeric(text).is_none(), "{}", text);
        }
    }

    #[test]
    fn test_query() {
        let sql = "SELECT name FROM people";
        let db_url = "postgres://postgres:123456@127.0.0.1:15432/demo_db";
        let rows = pg_query(db_url, sql, &IntMap::default());
        for key in rows.to_vec() {
            let value = rows.get(&key);
            println!("{}: {}", key, value.to_string());
        }
    }

    #[test]
    #[ignore]
    fn test_delete_row() {
        let sql = "delete from people where id = 11";
        let db_url = "postgres://postgres:123456@127.0.0.1:15432/demo_db";
        let _ = pg_execute(db_url, sql, &IntMap::default());
    }
}
