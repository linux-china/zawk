// nats (sync client) is deprecated in favor of async-nats, but still required here
#![allow(deprecated)]

use std::collections::HashMap;
use std::env;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use lazy_static::lazy_static;
use lettre::Transport;
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderName};
use serde::Serialize;
use url::Url;
use paho_mqtt::*;
use crate::runtime::{stdlib_warning, Str, StrMap};

pub fn local_ip() -> String {
    if let Ok(my_ip) = local_ip_address::local_ip() {
        return my_ip.to_string();
    }
    "127.0.0.1".to_owned()
}

/// Shared HTTP client: reuses the connection pool across calls, with explicit timeouts.
static HTTP_CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
});

pub(crate) fn http_get<'a>(url: &str, headers: &StrMap<'a, Str<'a>>) -> StrMap<'a, Str<'a>> {
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = HTTP_CLIENT.get(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    if let Ok(resp) = builder.send() {
        fill_response(resp, &resp_obj);
    } else {
        resp_obj.insert(Str::from("status"), Str::from("0"));
    }
    resp_obj
}


pub(crate) fn http_post<'a>(url: &str, headers: &StrMap<'a, Str<'a>>, body: &Str) -> StrMap<'a, Str<'a>> {
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = HTTP_CLIENT.post(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    let body_text = body.to_string();
    if !body_text.is_empty() {
        if !headers.contains(&Str::from("Content-Type")) {
            if (body_text.starts_with('{') && body_text.ends_with('}'))
                || (body_text.starts_with('[') && body_text.ends_with(']')) {
                builder = builder.header("Content-Type", "application/json");
            } else {
                builder = builder.header("Content-Type", "text/plain");
            }
        }
        builder = builder.body(body_text);
    }
    if let Ok(resp) = builder.send() {
        fill_response(resp, &resp_obj);
    } else {
        resp_obj.insert(Str::from("status"), Str::from("0"));
    }
    resp_obj
}

/// Request headers from an awk array; invalid header names or values are skipped with a warning.
fn convert_to_http_headers<'a>(headers: &StrMap<'a, Str<'a>>) -> HeaderMap {
    let mut request_headers = HeaderMap::new();
    for name in &headers.to_vec() {
        let name_text = name.to_string();
        let value_text = headers.get(name).to_string();
        match (HeaderName::from_bytes(name_text.as_bytes()), value_text.parse()) {
            (Ok(header_name), Ok(header_value)) => {
                request_headers.insert(header_name, header_value);
            }
            _ => stdlib_warning("http", format!("invalid HTTP header {:?}: {:?}", name_text, value_text)),
        }
    }
    request_headers
}

fn fill_response(resp: Response, resp_obj: &StrMap<Str>) {
    let status = resp.status();
    resp_obj.insert(Str::from("status"), Str::from(status.as_u16().to_string()));
    let response_headers = resp.headers();
    for (name, value) in response_headers.into_iter() {
        // header values are not necessarily ASCII
        resp_obj.insert(Str::from(name.to_string()), Str::from(String::from_utf8_lossy(value.as_bytes()).into_owned()));
    }
    if let Ok(body) = resp.text() {
        if !body.is_empty() {
            resp_obj.insert(Str::from("text"), Str::from(body.clone()));
        }
    }
}

// todo graceful shutdown
lazy_static! {
    static ref NATS_CONNECTIONS: Arc<Mutex<HashMap<String, nats::Connection>>> = Arc::new(Mutex::new(HashMap::new()));
    static ref MQTT_CONNECTIONS: Arc<Mutex<HashMap<String, paho_mqtt::Client>>> = Arc::new(Mutex::new(HashMap::new()));
}

/// Publish `body` to a NATS or MQTT topic, or show it as a desktop notification. Failures are
/// reported as warnings.
pub(crate) fn publish(namespace: &str, body: &str) {
    if let Err(msg) = try_publish(namespace, body) {
        stdlib_warning("publish", msg);
    }
}

fn try_publish(namespace: &str, body: &str) -> std::result::Result<(), String> {
    if namespace.starts_with("nats://") || namespace.starts_with("nats+tls://") {
        let url = Url::parse(namespace).map_err(|e| format!("invalid URL {:?}: {}", namespace, e))?;
        let host = url.host().ok_or_else(|| format!("missing host in {:?}", namespace))?;
        let topic = url.path().strip_prefix('/').unwrap_or(url.path()).to_string();
        let conn_url = if url.scheme().contains("tls") {
            format!("tls://{}:{}", host, url.port().unwrap_or(4443))
        } else {
            format!("{}:{}", host, url.port().unwrap_or(4222))
        };
        let mut pool = NATS_CONNECTIONS.lock().map_err(|e| e.to_string())?;
        if !pool.contains_key(&conn_url) {
            let nc = nats::connect(&conn_url).map_err(|e| format!("failed to connect to {}: {}", conn_url, e))?;
            pool.insert(conn_url.clone(), nc);
        }
        pool[&conn_url].publish(&topic, body).map_err(|e| e.to_string())
    } else if namespace.starts_with("mqtt://") || namespace.starts_with("mqtts://") {
        let url = Url::parse(namespace).map_err(|e| format!("invalid URL {:?}: {}", namespace, e))?;
        let topic = url.path().strip_prefix('/').unwrap_or(url.path()).to_string();
        if topic.is_empty() {
            return Err(format!("missing topic in {:?}", namespace));
        }
        let mut pool = MQTT_CONNECTIONS.lock().map_err(|e| e.to_string())?;
        if !pool.contains_key(namespace) {
            let cli = mqtt_connect(&url)?;
            pool.insert(namespace.to_string(), cli);
        }
        pool[namespace].publish(Message::new(topic, body, 0)).map_err(|e| e.to_string())
    } else {
        notify_rust::Notification::new()
            .summary(namespace)
            .body(body)
            .show()
            .map(drop)
            .map_err(|e| format!("failed to show notification: {}", e))
    }
}

fn mqtt_connect(url: &Url) -> std::result::Result<paho_mqtt::Client, String> {
    let schema = url.scheme();
    let host = url.host().ok_or_else(|| format!("missing host in {:?}", url.as_str()))?;
    let user_name = url.username();
    let password = url.password();
    let connection_url = if let Some(port) = url.port() {
        format!("{}://{}:{}", schema, host, port)
    } else {
        format!("{}://{}", schema, host)
    };
    let mut pairs = url.query_pairs();
    let version = pairs.find(|p| p.0 == "version");
    let mqtt_version = if let Some((_key, version)) = version {
        if version.contains("3.1.1") {
            MQTT_VERSION_3_1_1
        } else if version.contains("3.1") {
            MQTT_VERSION_3_1
        } else {
            MQTT_VERSION_5
        }
    } else {
        MQTT_VERSION_5
    };
    let client_opts = CreateOptionsBuilder::new()
        .mqtt_version(mqtt_version)
        .server_uri(&connection_url)
        .finalize();
    // Connect options
    let mut builder = ConnectOptionsBuilder::new();
    let mut conn_options_builder = builder.clean_start(true);
    if schema == "mqtts" {
        conn_options_builder = conn_options_builder.ssl_options(SslOptions::default());
    }
    if !user_name.is_empty() {
        if let Some(password) = password {
            conn_options_builder = conn_options_builder.user_name(user_name).password(password);
        } else {
            conn_options_builder = conn_options_builder.password(user_name); // JWT style
        }
    }
    let cli = Client::new(client_opts).map_err(|e| format!("failed to create MQTT client: {}", e))?;
    cli.connect(conn_options_builder.finalize())
        .map_err(|e| format!("failed to connect to {}: {}", connection_url, e))?;
    Ok(cli)
}

#[derive(Debug, Serialize)]
struct MailerSendRequest {
    from: MailAddress,
    to: Vec<MailAddress>,
    subject: String,
    text: String,
}

#[derive(Debug, Serialize)]
struct MailAddress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

impl MailAddress {
    pub fn new(email: &str) -> Self {
        MailAddress {
            name: None,
            email: email.to_string(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ResendRequest {
    from: String,
    to: Vec<String>,
    subject: String,
    text: String,
}

pub fn send_mail(from: &str, to: &str, subject: &str, text: &str) {
    let (api_url, api_key) = if let Ok(api_key) = env::var("MLSN_API_KEY") {
        ("https://api.mailersend.com/v1/email".to_owned(), api_key)
    } else if let Ok(api_key) = env::var("RESEND_API_KEY") {
        ("https://api.resend.com/emails".to_owned(), api_key)
    } else {
        ("".to_owned(), "".to_owned())
    };
    if env::var("DRY_RUN").is_ok() {
        println!("====DRY_RUN MODE====");
        println!("API URL: {}", api_url);
        return;
    }
    if !api_url.is_empty() {
        let mut builder = HTTP_CLIENT.post(&api_url)
            .header("Authorization", format!("Bearer {}", api_key));
        if api_url.starts_with("https://api.resend.com") {
            let receivers: Vec<String> = to.split(',').map(|s| s.to_string()).collect();
            let req = ResendRequest {
                from: from.to_string(),
                to: receivers,
                subject: subject.to_string(),
                text: text.to_string(),
            };
            builder = builder.json(&req);
        } else if api_url.starts_with("https://api.mailersend.com") {
            let receivers = to.split(',').map(|email| MailAddress::new(email)).collect();
            let req = MailerSendRequest {
                from: MailAddress::new(from),
                to: receivers,
                subject: subject.to_string(),
                text: text.to_string(),
            };
            builder = builder.json(&req);
        }
        match builder.send() {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().unwrap_or_default();
                    eprintln!("Failed to send mail via {}: HTTP {} {}", api_url, status, body);
                }
            }
            Err(e) => eprintln!("Failed to send mail via {}: {}", api_url, e),
        }
    }
}

/// Send a mail through the SMTP server `url`. Failures are reported as warnings.
pub fn smtp_send(url: &str, from: &str, to: &str, subject: &str, text: &str) {
    if let Err(msg) = try_smtp_send(url, from, to, subject, text) {
        stdlib_warning("smtp_send", msg);
    }
}

fn try_smtp_send(url: &str, from: &str, to: &str, subject: &str, text: &str) -> std::result::Result<(), String> {
    let address = |text: &str| {
        text.trim().parse::<lettre::message::Mailbox>()
            .map_err(|e| format!("invalid mail address {:?}: {}", text, e))
    };
    let mut builder = lettre::Message::builder().from(address(from)?).subject(subject);
    for email_address in to.split(",") {
        builder = builder.to(address(email_address)?);
    }
    let email = builder
        .header(lettre::message::header::ContentType::TEXT_PLAIN)
        .body(String::from(text))
        .map_err(|e| e.to_string())?;
    let mailer = lettre::SmtpTransport::from_url(url)
        .map_err(|e| format!("invalid SMTP URL: {}", e))?
        .build();
    mailer.send(&email).map(drop).map_err(|e| format!("failed to send mail: {}", e))
}

#[cfg(test)]
mod tests {
    use local_ip_address::local_ip;
    use super::*;

    #[test]
    fn test_local_ip() {
        let my_local_ip = local_ip().unwrap();
        println!("This is my local IP address: {:?}", my_local_ip);
    }

    #[test]
    fn test_http_get() {
        let url = "https://httpbin.org/ip";
        let headers: StrMap<Str> = StrMap::default();
        let resp = http_get(url, &headers);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    fn test_http_post() {
        let url = "https://httpbin.org/post";
        let headers: StrMap<Str> = StrMap::default();
        let body = Str::from(r#"{"status": "ok"}"#);
        let resp = http_post(url, &headers, &body);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    #[ignore]
    fn test_publish_nats() {
        let url = "nats://localhost:4222/topic1";
        publish(url, "Hello World!");
    }

    #[test]
    #[ignore]
    fn test_publish_mqtt() {
        let url = "mqtt://localhost:1883/topic1";
        publish(url, "Hello World!");
    }

    #[test]
    #[ignore]
    fn test_send_email() {
        dotenv::dotenv().ok();
        let from = "support@trial-3zxk54v3ykzgjy6v.mlsender.net";
        let to = "linux_china@hotmail.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        send_mail(from, to, subject, text);
    }

    #[test]
    #[ignore]
    fn test_send_smtp() {
        dotenv::dotenv().ok();
        let smtp_url = env::var("SMTP_URL").unwrap();
        let from = "libing.chen@example";
        let to = "linux_china@example.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        smtp_send(&smtp_url, from, to, subject, text);
    }
}
