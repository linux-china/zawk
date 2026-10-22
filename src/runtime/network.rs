use std::collections::HashMap;
use std::env;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use lettre::Transport;
use reqwest::blocking::Response;
use reqwest::header::{HeaderMap, HeaderName};
use serde::Serialize;
use url::Url;
use crate::runtime::{set_errno, stdlib_warning, Str, StrMap};

/// `url` with the password in its userinfo replaced by `***`, for error messages and logs:
/// `redis://user:secret@host/x` becomes `redis://user:***@host/x`. A userinfo without `:` (such
/// as a token in `nats://token@host`) is replaced as a whole.
pub(crate) fn redact_url(url: &str) -> std::borrow::Cow<'_, str> {
    let start = url.find("://").map_or(0, |i| i + 3);
    let end = url[start..].find(['/', '?', '#']).map_or(url.len(), |i| start + i);
    match url[start..end].rfind('@') {
        Some(at) => {
            let userinfo = &url[start..start + at];
            let shown = match userinfo.split_once(':') {
                Some((user, _)) => format!("{}:***", user),
                None => "***".to_owned(),
            };
            format!("{}{}{}", &url[..start], shown, &url[start + at..]).into()
        }
        None => url.into(),
    }
}

pub fn local_ip() -> String {
    if let Ok(my_ip) = local_ip_address::local_ip() {
        return my_ip.to_string();
    }
    "127.0.0.1".to_owned()
}

/// Shared HTTP client: reuses the connection pool across calls, with explicit timeouts.
static HTTP_CLIENT: LazyLock<reqwest::blocking::Client> = LazyLock::new(|| {
    crate::runtime::tls::install_default_provider();
    reqwest::blocking::Client::builder()
        .user_agent(concat!("zawk/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("Failed to build HTTP client")
});

/// GET `url` with the shared client for functions that use the body as data (`read_all`,
/// `parse_prometheus`, JWKS, `-f URL`): error statuses (404, proxy errors, ...) are errors, so an
/// error page is never read as content.
pub(crate) fn fetch(url: &str) -> reqwest::Result<Response> {
    HTTP_CLIENT.get(url).send().and_then(|resp| resp.error_for_status())
}

/// The body at `url` as text, see `fetch`; the error leaves out the URL, which callers report.
pub(crate) fn fetch_text(url: &str) -> Result<String, String> {
    fetch(url).and_then(|resp| resp.text()).map_err(|e| e.without_url().to_string())
}

pub(crate) fn http_get<'a>(url: &str, headers: &StrMap<'a, Str<'a>>) -> StrMap<'a, Str<'a>> {
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = HTTP_CLIENT.get(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    match builder.send() {
        Ok(resp) => fill_response(resp, &resp_obj),
        Err(e) => {
            // status 0 tells the failure; ERRNO tells why, as for other stdlib functions
            set_errno(format!("http_get: {}", e));
            resp_obj.insert(Str::from("status"), Str::from("0"));
        }
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
    match builder.send() {
        Ok(resp) => fill_response(resp, &resp_obj),
        Err(e) => {
            // status 0 tells the failure; ERRNO tells why, as for other stdlib functions
            set_errno(format!("http_post: {}", e));
            resp_obj.insert(Str::from("status"), Str::from("0"));
        }
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
    let response_headers = resp.headers();
    for (name, value) in response_headers.into_iter() {
        // header values are not necessarily ASCII
        resp_obj.insert(Str::from(name.to_string()), Str::from(String::from_utf8_lossy(value.as_bytes()).into_owned()));
    }
    resp_obj.insert(Str::from("status"), Str::from(status.as_u16().to_string()));
    if let Ok(body) = resp.text() {
        if !body.is_empty() {
            resp_obj.insert(Str::from("text"), Str::from(body.clone()));
        }
    }
}

/// All NATS requests run on this runtime. Its worker thread keeps driving the connections (PING/PONG,
/// reconnects) between calls, so pooled clients are not dropped by the server while the program is busy.
pub(crate) static NATS_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("zawk-nats")
        .enable_all()
        .build()
        .expect("failed to build NATS runtime")
});

// todo graceful shutdown
static NATS_CONNECTIONS: LazyLock<Mutex<HashMap<String, async_nats::Client>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The pooled NATS client of `conn_url` (`nats://[user:password@]host[:port]` or `tls://...`), connected if needed.
pub(crate) fn nats_client(conn_url: &str) -> std::result::Result<async_nats::Client, String> {
    let mut pool = NATS_CONNECTIONS.lock().map_err(|e| e.to_string())?;
    if let Some(client) = pool.get(conn_url) {
        return Ok(client.clone());
    }
    let mut options = async_nats::ConnectOptions::new().connection_timeout(Duration::from_secs(10));
    // async_nats ignores the userinfo of the server URL: pass `user:password@` / `token@` explicitly
    if let Ok(url) = Url::parse(conn_url) {
        let decode = |s: &str| urlencoding::decode(s).map(|s| s.into_owned()).unwrap_or_else(|_| s.to_string());
        if !url.username().is_empty() {
            options = match url.password() {
                Some(password) => options.user_and_password(decode(url.username()), decode(password)),
                None => options.token(decode(url.username())),
            };
        }
    }
    let client = NATS_RUNTIME
        .block_on(options.connect(conn_url))
        .map_err(|e| format!("failed to connect to {}: {}", redact_url(conn_url), e))?;
    pool.insert(conn_url.to_string(), client.clone());
    Ok(client)
}
static MQTT_CONNECTIONS: LazyLock<Arc<Mutex<HashMap<String, MqttClient>>>> = LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

/// Publish `body` to a NATS or MQTT topic, or show it as a desktop notification. Failures are
/// reported as warnings.
pub(crate) fn publish(namespace: &str, body: &str) {
    if let Err(msg) = try_publish(namespace, body) {
        stdlib_warning("publish", msg);
    }
}

fn try_publish(namespace: &str, body: &str) -> std::result::Result<(), String> {
    if namespace.starts_with("nats://") || namespace.starts_with("nats+tls://") {
        let url = Url::parse(namespace).map_err(|e| format!("invalid URL {:?}: {}", redact_url(namespace), e))?;
        if url.host().is_none() {
            return Err(format!("missing host in {:?}", redact_url(namespace)));
        }
        let topic = url.path().strip_prefix('/').unwrap_or(url.path()).to_string();
        // keep the userinfo (user:password@ / token@) and let async_nats default the port, same as `kv_*`
        let conn_url = url[..url::Position::BeforePath].replacen("nats+tls://", "tls://", 1);
        let client = nats_client(&conn_url)?;
        // publish only queues the message: flush so it is not lost when the script exits right after
        NATS_RUNTIME.block_on(async {
            client.publish(topic, body.to_string().into()).await.map_err(|e| format!("failed to publish: {}", e))?;
            client.flush().await.map_err(|e| format!("failed to flush: {}", e))
        })
    } else if namespace.starts_with("mqtt://") || namespace.starts_with("mqtts://") {
        let url = Url::parse(namespace).map_err(|e| format!("invalid URL {:?}: {}", redact_url(namespace), e))?;
        let topic = url.path().strip_prefix('/').unwrap_or(url.path()).to_string();
        if topic.is_empty() {
            return Err(format!("missing topic in {:?}", redact_url(namespace)));
        }
        let mut pool = MQTT_CONNECTIONS.lock().map_err(|e| e.to_string())?;
        let reused = pool.contains_key(namespace);
        let mut cli = match pool.remove(namespace) {
            Some(cli) => cli,
            None => mqtt_connect(&url)?,
        };
        let mut result = cli.publish(&topic, body);
        if result.is_err() && reused {
            // the pooled connection may have been dropped by the broker while idle: reconnect once
            cli = mqtt_connect(&url)?;
            result = cli.publish(&topic, body);
        }
        if result.is_ok() {
            pool.insert(namespace.to_string(), cli);
        }
        result
    } else {
        notify_rust::Notification::new()
            .summary(namespace)
            .body(body)
            .show()
            .map(drop)
            .map_err(|e| format!("failed to show notification: {}", e))
    }
}

/// Timeout to connect to the broker and flush a published message.
const MQTT_TIMEOUT: Duration = Duration::from_secs(10);

/// MQTT client with its (synchronously driven) event loop. MQTT 3.1/3.1.1 use the v4 protocol, otherwise v5.
enum MqttClient {
    V4(rumqttc::Client, rumqttc::Connection),
    V5(rumqttc::v5::Client, rumqttc::v5::Connection),
}

impl MqttClient {
    /// Publish `body` with QoS 0, then drive the event loop (connecting if needed) until the message is
    /// written to the network, so it is not lost when the script exits right after.
    fn publish(&mut self, topic: &str, body: &str) -> std::result::Result<(), String> {
        macro_rules! publish_and_flush {
            ($client:expr, $conn:expr, $qos:expr, $event:path, $payload:expr) => {{
                $client.publish(topic, $qos, false, $payload).map_err(|e| format!("failed to publish: {}", e))?;
                let deadline = std::time::Instant::now() + MQTT_TIMEOUT;
                loop {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    match $conn.recv_timeout(remaining) {
                        Ok(Ok($event(rumqttc::Outgoing::Publish(_)))) => return Ok(()),
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => return Err(format!("MQTT connection error: {}", e)),
                        Err(_) => return Err("timed out publishing MQTT message".to_string()),
                    }
                }
            }};
        }
        match self {
            MqttClient::V4(client, conn) => {
                publish_and_flush!(client, conn, rumqttc::QoS::AtMostOnce, rumqttc::Event::Outgoing, body.as_bytes().to_vec())
            }
            MqttClient::V5(client, conn) => {
                publish_and_flush!(client, conn, rumqttc::v5::mqttbytes::QoS::AtMostOnce, rumqttc::v5::Event::Outgoing, body.to_string())
            }
        }
    }
}

fn mqtt_connect(url: &Url) -> std::result::Result<MqttClient, String> {
    let tls = url.scheme() == "mqtts";
    let host = match url.host().ok_or_else(|| format!("missing host in {:?}", redact_url(url.as_str())))? {
        url::Host::Domain(domain) => domain.to_string(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    let port = url.port().unwrap_or(if tls { 8883 } else { 1883 });
    // MQTT 3.1.1 limits client ids to 23 characters
    let client_id = format!("zawk-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    // username + password, or a token (JWT style) as the password only; userinfo is percent-encoded in the URL
    let decode = |s: &str| urlencoding::decode(s).map(|s| s.into_owned()).unwrap_or_else(|_| s.to_string());
    let user_name = url.username();
    let credentials = if user_name.is_empty() {
        None
    } else if let Some(password) = url.password() {
        Some((decode(user_name), decode(password)))
    } else {
        Some((String::new(), decode(user_name)))
    };
    let is_v5 = !url.query_pairs().any(|(key, version)| key == "version" && version.contains("3.1"));
    let transport = if tls { rumqttc::Transport::tls_with_config(crate::runtime::tls::client_config()?.into()) } else { rumqttc::Transport::tcp() };
    let keep_alive = Duration::from_secs(60);
    if is_v5 {
        let mut options = rumqttc::v5::MqttOptions::new(client_id, host, port);
        options.set_keep_alive(keep_alive).set_clean_start(true).set_transport(transport);
        if let Some((user_name, password)) = credentials {
            options.set_credentials(user_name, password);
        }
        let (client, conn) = rumqttc::v5::Client::new(options, 10);
        Ok(MqttClient::V5(client, conn))
    } else {
        let mut options = rumqttc::MqttOptions::new(client_id, host, port);
        options.set_keep_alive(keep_alive).set_clean_session(true).set_transport(transport);
        if let Some((user_name, password)) = credentials {
            options.set_credentials(user_name, password);
        }
        let (client, conn) = rumqttc::Client::new(options, 10);
        Ok(MqttClient::V4(client, conn))
    }
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

/// Send a mail. The transport is the SMTP server `smtp_url` when given, then the `SMTP_URL`,
/// `MLSN_API_KEY` (MailerSend) or `RESEND_API_KEY` (Resend) environment variable. Failures are
/// reported as warnings.
pub fn send_mail(from: &str, to: &str, subject: &str, text: &str, smtp_url: &str) {
    if let Err(msg) = try_send_mail(from, to, subject, text, smtp_url) {
        stdlib_warning("send_mail", msg);
    }
}

fn try_send_mail(from: &str, to: &str, subject: &str, text: &str, smtp_url: &str) -> std::result::Result<(), String> {
    let smtp_url = if smtp_url.is_empty() { env::var("SMTP_URL").unwrap_or_default() } else { smtp_url.to_owned() };
    let (api_url, api_key) = if !smtp_url.is_empty() {
        (smtp_url, String::new())
    } else if let Ok(api_key) = env::var("MLSN_API_KEY") {
        ("https://api.mailersend.com/v1/email".to_owned(), api_key)
    } else if let Ok(api_key) = env::var("RESEND_API_KEY") {
        ("https://api.resend.com/emails".to_owned(), api_key)
    } else {
        return Err("no mail transport: pass smtp_url or set SMTP_URL, MLSN_API_KEY or RESEND_API_KEY".to_owned());
    };
    if env::var("DRY_RUN").is_ok() {
        println!("====DRY_RUN MODE====");
        // Do not print SMTP credentials: smtps://user:password@host:465
        println!("API URL: {}", redact_url(&api_url));
        return Ok(());
    }
    if api_url.starts_with("smtp://") || api_url.starts_with("smtps://") {
        return try_smtp_send(&api_url, from, to, subject, text);
    }
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
    } else {
        let receivers = to.split(',').map(|email| MailAddress::new(email)).collect();
        let req = MailerSendRequest {
            from: MailAddress::new(from),
            to: receivers,
            subject: subject.to_string(),
            text: text.to_string(),
        };
        builder = builder.json(&req);
    }
    let resp = builder.send().map_err(|e| format!("failed to send mail via {}: {}", redact_url(&api_url), e))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        return Err(format!("failed to send mail via {}: HTTP {} {}", redact_url(&api_url), status, body));
    }
    Ok(())
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
    fn test_redact_url() {
        assert_eq!(redact_url("redis://user:s3cret@127.0.0.1:1/h"), "redis://user:***@127.0.0.1:1/h");
        assert_eq!(redact_url("redis://:s3cret@127.0.0.1"), "redis://:***@127.0.0.1");
        assert_eq!(redact_url("nats://token@127.0.0.1:4222"), "nats://***@127.0.0.1:4222");
        assert_eq!(redact_url("smtps://u%40x.com:p@ss@smtp.x.com:465"), "smtps://u%40x.com:***@smtp.x.com:465");
        assert_eq!(redact_url("http://127.0.0.1/a@b?x=@"), "http://127.0.0.1/a@b?x=@");
        assert_eq!(redact_url("127.0.0.1:4222"), "127.0.0.1:4222");
    }

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
        dotenvy::dotenv().ok();
        let from = "support@trial-3zxk54v3ykzgjy6v.mlsender.net";
        let to = "linux_china@hotmail.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        send_mail(from, to, subject, text, "");
    }

    #[test]
    #[ignore]
    fn test_send_smtp() {
        dotenvy::dotenv().ok();
        let smtp_url = env::var("SMTP_URL").unwrap();
        let from = "libing.chen@example";
        let to = "linux_china@example.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        send_mail(from, to, subject, text, &smtp_url);
    }
}
