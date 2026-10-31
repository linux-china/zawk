use crate::runtime::conn_pool::{ConnPool, ConnSlots};
use crate::runtime::splitter::kafka;
use crate::runtime::{Str, StrMap, stdlib_warning};
use confluent_kafka::common::serialization::StringSerializer;
use confluent_kafka::producer::{KafkaProducer, Producer, ProducerConfig, ProducerRecord};
use lettre::Transport;
use reqwest::blocking::Response;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName};
use serde::Serialize;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;
use url::Url;

/// How long connecting to a server (http input, redis, mysql, postgres) may take, so that an
/// unreachable host fails instead of waiting for the TCP default of several minutes.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a streaming connection (http input, redis) may stay silent while a reply is expected.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Query parameters whose values are credentials, such as libpq's `?password=...`.
const SECRET_PARAMS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "sslpassword",
    "token",
    "access_token",
    "secret",
    "api_key",
    "apikey",
];

/// The byte offsets of the userinfo of `url` and of the `@` that ends it: `(3, 13)` for
/// `s://user:pw@host`. A raw password may contain `/`, `?`, `#` or `@` (`u:p/ss@h/db#t`): when
/// the authority (up to the first `/`, `?` or `#`) has no `@`, the userinfo ends at the last `@`
/// before the first `#` that follows an `@`, if it has a `:` and its user part has none of
/// `/?#` and no whitespace, so that an `@` in a path, query or SQL is not taken for one.
pub(crate) fn userinfo_range(url: &str) -> Option<(usize, usize)> {
    let start = url.find("://").map_or(0, |i| i + 3);
    let rest = &url[start..];
    let authority = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if let Some(at) = rest[..authority].rfind('@') {
        return Some((start, start + at));
    }
    let first_at = rest.find('@')?;
    let region = rest[first_at..]
        .find('#')
        .map_or(rest.len(), |i| first_at + i);
    let at = rest[..region].rfind('@')?;
    let userinfo = &rest[..at];
    let (user, _) = userinfo.split_once(':')?;
    if user.contains(['/', '?', '#']) || userinfo.contains(char::is_whitespace) {
        return None;
    }
    Some((start, start + at))
}

/// `url` with its credentials replaced by `***`, for error messages and logs:
/// `redis://user:secret@host/x` becomes `redis://user:***@host/x`. A userinfo without `:` (such
/// as a token in `nats://token@host`) is replaced as a whole, and so are the values of query
/// parameters named in `SECRET_PARAMS` (`postgres://h/db?password=secret#t` becomes
/// `postgres://h/db?password=***#t`). The fragment (a table or query of a database URL) is kept.
/// A raw password with `/`, `?`, `#` or `@` is found as `userinfo_range` tells.
pub(crate) fn redact_url(url: &str) -> std::borrow::Cow<'_, str> {
    let mut out = String::new();
    let mut copied = 0;
    let mut end = url.find("://").map_or(0, |i| i + 3);
    if let Some((start, at)) = userinfo_range(url) {
        let userinfo = &url[start..at];
        out.push_str(&url[..start]);
        match userinfo.split_once(':') {
            Some((user, _)) => {
                out.push_str(user);
                out.push_str(":***");
            }
            None => out.push_str("***"),
        }
        copied = at;
        end = at;
    }
    end = url[end..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| end + i);
    let fragment = url[end..].find('#').map_or(url.len(), |i| end + i);
    if let Some(q) = url[end..fragment].find('?') {
        let query_start = end + q + 1;
        let mut pos = query_start;
        for param in url[query_start..fragment].split('&') {
            if let Some((name, value)) = param.split_once('=') {
                if !value.is_empty() && SECRET_PARAMS.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                    let value_start = pos + name.len() + 1;
                    out.push_str(&url[copied..value_start]);
                    out.push_str("***");
                    copied = value_start + value.len();
                }
            }
            pos += param.len() + 1;
        }
    }
    if copied == 0 {
        return url.into();
    }
    out.push_str(&url[copied..]);
    out.into()
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
    HTTP_CLIENT
        .get(url)
        .send()
        .and_then(|resp| resp.error_for_status())
}

/// The body at `url` as text, see `fetch`; the error leaves out the URL, which callers report.
pub(crate) fn fetch_text(url: &str) -> Result<String, String> {
    fetch(url)
        .and_then(|resp| resp.text())
        .map_err(|e| e.without_url().to_string())
}

pub(crate) fn http_get<'a>(url: &str, headers: &StrMap<'a, Str<'a>>) -> StrMap<'a, Str<'a>> {
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = HTTP_CLIENT.get(url);
    if headers.len() > 0 {
        builder = builder.headers(convert_to_http_headers(headers));
    }
    match builder.send() {
        Ok(resp) => fill_response("http_get", url, resp, &resp_obj),
        Err(e) => {
            // status 0 tells the failure; a warning and ERRNO tell why, as for other stdlib functions
            // reqwest errors carry the full URL, credentials included: report it redacted
            stdlib_warning(
                "http_get",
                format!("{}: {}", redact_url(url), e.without_url()),
            );
            resp_obj.insert(Str::from("status"), Str::from("0"));
        }
    }
    resp_obj
}

pub(crate) fn http_post<'a>(
    url: &str,
    headers: &StrMap<'a, Str<'a>>,
    body: &Str,
) -> StrMap<'a, Str<'a>> {
    let resp_obj: StrMap<Str> = StrMap::default();
    let mut builder = HTTP_CLIENT.post(url);
    // HeaderMap keys are case-insensitive, so `content-type` from the user counts as set
    let request_headers = convert_to_http_headers(headers);
    let has_content_type = request_headers.contains_key(CONTENT_TYPE);
    if !request_headers.is_empty() {
        builder = builder.headers(request_headers);
    }
    let body_text = body.to_string();
    if !body_text.is_empty() {
        if !has_content_type {
            if (body_text.starts_with('{') && body_text.ends_with('}'))
                || (body_text.starts_with('[') && body_text.ends_with(']'))
            {
                builder = builder.header(CONTENT_TYPE, "application/json");
            } else {
                builder = builder.header(CONTENT_TYPE, "text/plain");
            }
        }
        builder = builder.body(body_text);
    }
    match builder.send() {
        Ok(resp) => fill_response("http_post", url, resp, &resp_obj),
        Err(e) => {
            // status 0 tells the failure; a warning and ERRNO tell why, as for other stdlib functions
            // reqwest errors carry the full URL, credentials included: report it redacted
            stdlib_warning(
                "http_post",
                format!("{}: {}", redact_url(url), e.without_url()),
            );
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
        match (
            HeaderName::from_bytes(name_text.as_bytes()),
            value_text.parse(),
        ) {
            (Ok(header_name), Ok(header_value)) => {
                request_headers.insert(header_name, header_value);
            }
            _ => stdlib_warning(
                "http",
                format!("invalid HTTP header {:?}: {:?}", name_text, value_text),
            ),
        }
    }
    request_headers
}

fn fill_response(func: &str, url: &str, resp: Response, resp_obj: &StrMap<Str>) {
    let status = resp.status();
    for (name, value) in merge_headers(resp.headers()) {
        resp_obj.insert(Str::from(name), Str::from(value));
    }
    // set after the headers, and even for an empty body, so a header named `status`/`text` cannot take their place
    match resp.text() {
        Ok(text) => {
            resp_obj.insert(Str::from("status"), Str::from(status.as_u16().to_string()));
            resp_obj.insert(Str::from("text"), Str::from(text));
        }
        Err(e) => {
            // a body cut off by the timeout or a reset connection is a failure, not an empty success:
            // status 0, a warning and ERRNO as for a failed send
            stdlib_warning(
                func,
                format!(
                    "{}: failed to read response body: {}",
                    redact_url(url),
                    e.without_url()
                ),
            );
            resp_obj.insert(Str::from("status"), Str::from("0"));
            resp_obj.insert(Str::from("text"), Str::from(""));
        }
    }
}

/// Response headers by name: the values of a repeated header are joined with `, `, or with `\n`
/// for `Set-Cookie`, whose values may contain commas.
fn merge_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .keys()
        .map(|name| {
            let separator = if name == reqwest::header::SET_COOKIE {
                "\n"
            } else {
                ", "
            };
            // header values are not necessarily ASCII
            let values: Vec<String> = headers
                .get_all(name)
                .iter()
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
                .collect();
            (name.to_string(), values.join(separator))
        })
        .collect()
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

/// How long a NATS request waits for the server, e.g. while the client reconnects after losing it.
const NATS_TIMEOUT: Duration = Duration::from_secs(10);

/// Run `request` on [`NATS_RUNTIME`]. A client reconnects forever after losing the server and its
/// requests wait for that, so every request that needs an answer from the server must go through here.
pub(crate) fn nats_block_on<T, E: std::fmt::Display>(
    request: impl Future<Output = std::result::Result<T, E>>,
) -> std::result::Result<T, String> {
    NATS_RUNTIME
        .block_on(async { tokio::time::timeout(NATS_TIMEOUT, request).await })
        .map_err(|_| format!("timed out after {:?} waiting for the NATS server", NATS_TIMEOUT))?
        .map_err(|e| e.to_string())
}

pub(crate) static KAFKA_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_name("zawk-kafka")
        .enable_all()
        .build()
        .expect("failed to build kafka runtime")
});

/// Whether `n` is 1, 10, 100, ...: warn at these counts only, not to flood stderr.
pub(crate) fn is_power_of_ten(mut n: u64) -> bool {
    while n != 0 && n.is_multiple_of(10) {
        n /= 10;
    }
    n == 1
}

// todo graceful shutdown
static NATS_CONNECTIONS: LazyLock<ConnSlots<async_nats::Client>> = LazyLock::new(ConnSlots::new);

/// The pooled NATS client of `conn_url` (`nats://[user:password@]host[:port]` or `tls://...`), connected if needed.
pub(crate) fn nats_client(conn_url: &str) -> std::result::Result<async_nats::Client, String> {
    NATS_CONNECTIONS.with_conn(conn_url, || nats_connect(conn_url), |client| Ok(client.clone()))
}

fn nats_connect(conn_url: &str) -> std::result::Result<async_nats::Client, String> {
    let mut options = async_nats::ConnectOptions::new()
        .connection_timeout(Duration::from_secs(10))
        .event_callback(|event| async move {
            // a subscriber that cannot keep up: its buffer is full and the message is dropped
            if let async_nats::Event::SlowConsumer(_) = event {
                static DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let dropped = DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if is_power_of_ten(dropped) {
                    eprintln_ignore!(
                        "zawk: warning: NATS messages dropped ({} so far): the program cannot keep up with the subject",
                        dropped
                    );
                }
            }
        });
    // async_nats ignores the userinfo of the server URL: pass `user:password@` / `token@` explicitly
    if let Ok(url) = Url::parse(conn_url) {
        let decode = |s: &str| {
            urlencoding::decode(s)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| s.to_string())
        };
        if !url.username().is_empty() {
            options = match url.password() {
                Some(password) => {
                    options.user_and_password(decode(url.username()), decode(password))
                }
                None => options.token(decode(url.username())),
            };
        }
    }
    NATS_RUNTIME
        .block_on(options.connect(conn_url))
        .map_err(|e| format!("failed to connect to {}: {}", redact_url(conn_url), e))
}

/// How long `publish` waits for the metadata of a Kafka topic, e.g. when the broker cannot be reached.
const KAFKA_MAX_BLOCK_MS: &str = "10000";

type KafkaStringProducer = KafkaProducer<String, String>;

static KAFKA_PRODUCERS: LazyLock<Mutex<HashMap<String, Arc<KafkaStringProducer>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The pooled Kafka producer of `bootstrap_servers` (`host:port`), created if needed.
fn kafka_producer(
    bootstrap_servers: &str,
) -> std::result::Result<Arc<KafkaStringProducer>, String> {
    let mut pool = KAFKA_PRODUCERS.lock().map_err(|e| e.to_string())?;
    if let Some(producer) = pool.get(bootstrap_servers) {
        return Ok(producer.clone());
    }
    let props = HashMap::from([
        (
            ProducerConfig::BOOTSTRAP_SERVERS_CONFIG.to_string(),
            bootstrap_servers.to_string(),
        ),
        (ProducerConfig::ACKS_CONFIG.to_string(), "all".to_string()),
        // `publish` waits for each ack: there is no batch to wait for
        (ProducerConfig::LINGER_MS_CONFIG.to_string(), "0".to_string()),
        (
            ProducerConfig::MAX_BLOCK_MS_CONFIG.to_string(),
            KAFKA_MAX_BLOCK_MS.to_string(),
        ),
    ]);
    // the producer starts its sender task on the current runtime
    let producer = KAFKA_RUNTIME.block_on(async {
        let config = ProducerConfig::new(&props).map_err(|e| e.to_string())?;
        KafkaProducer::new(config, StringSerializer::new(), StringSerializer::new())
            .map_err(|e| e.to_string())
    })?;
    let producer = Arc::new(producer);
    pool.insert(bootstrap_servers.to_string(), producer.clone());
    Ok(producer)
}

static MQTT_CONNECTIONS: LazyLock<ConnPool<MqttClient>> = LazyLock::new(ConnPool::new);

/// Publish `body` to a NATS, Kafka or MQTT topic, or show it as a desktop notification. Failures are
/// reported as warnings.
pub(crate) fn publish(namespace: &str, body: &str) {
    if let Err(msg) = try_publish(namespace, body) {
        stdlib_warning("publish", msg);
    }
}

fn try_publish(namespace: &str, body: &str) -> std::result::Result<(), String> {
    // an unknown scheme or a malformed URL is an error: never fall back to another transport
    let scheme = match namespace.split_once("://") {
        Some((scheme, _)) => scheme.to_ascii_lowercase(),
        None => {
            return Err(format!(
                "invalid URL {:?}, expected nats://, nats+tls://, kafka://, mqtt://, mqtts:// or desktop://",
                redact_url(namespace),
            ));
        }
    };
    if scheme == "nats" || scheme == "nats+tls" {
        let url = Url::parse(namespace)
            .map_err(|e| format!("invalid URL {:?}: {}", redact_url(namespace), e))?;
        if url.host().is_none() {
            return Err(format!("missing host in {:?}", redact_url(namespace)));
        }
        let topic = url
            .path()
            .strip_prefix('/')
            .unwrap_or(url.path())
            .to_string();
        // keep the userinfo (user:password@ / token@) and let async_nats default the port, same as `kv_*`
        let conn_url = url[..url::Position::BeforePath].replacen("nats+tls://", "tls://", 1);
        let client = nats_client(&conn_url)?;
        // publish only queues the message: flush so it is not lost when the script exits right after
        nats_block_on(async {
            client
                .publish(topic, body.to_string().into())
                .await
                .map_err(|e| format!("failed to publish: {}", e))?;
            client
                .flush()
                .await
                .map_err(|e| format!("failed to flush: {}", e))
        })
    } else if scheme == "mqtt" || scheme == "mqtts" {
        let url = Url::parse(namespace)
            .map_err(|e| format!("invalid URL {:?}: {}", redact_url(namespace), e))?;
        let topic = url
            .path()
            .strip_prefix('/')
            .unwrap_or(url.path())
            .to_string();
        if topic.is_empty() {
            return Err(format!("missing topic in {:?}", redact_url(namespace)));
        }
        // one connection per broker (scheme://userinfo@host:port) and protocol options (query), shared by all topics
        let key = match url.query() {
            Some(query) => format!("{}?{}", &url[..url::Position::BeforePath], query),
            None => url[..url::Position::BeforePath].to_string(),
        };
        let (mut cli, reused) = match MQTT_CONNECTIONS.take(&key) {
            Some(cli) => (cli, true),
            None => (mqtt_connect(&url)?, false),
        };
        let mut result = cli.publish(&topic, body);
        if result.is_err() && reused {
            // the pooled connection may have been dropped by the broker while idle: reconnect once
            cli = mqtt_connect(&url)?;
            result = cli.publish(&topic, body);
        }
        if result.is_ok() {
            MQTT_CONNECTIONS.put(&key, cli);
        }
        result
    } else if scheme == "kafka" {
        let url = Url::parse(namespace)
            .map_err(|e| format!("invalid URL {:?}: {}", redact_url(namespace), e))?;
        let (bootstrap_servers, topic) = kafka::endpoint(&url, namespace)?;
        let key = kafka::publish_key(&url, namespace)?;
        let producer = kafka_producer(&bootstrap_servers)?;
        // send only queues the record: wait for the ack so it is not lost when the script exits right after
        KAFKA_RUNTIME.block_on(async {
            let record = ProducerRecord::with_key(topic, key, Some(body.to_string()));
            let delivery = producer
                .send(record)
                .await
                .map_err(|e| format!("failed to publish: {}", e))?;
            delivery
                .get()
                .await
                .map(drop)
                .map_err(|e| format!("failed to publish: {}", e))
        })
    } else if scheme == "desktop" {
        // desktop://title: the title is shown in the notification center, so mask any secret in it
        let redacted = redact_url(namespace);
        let title = redacted.split_once("://").map_or("", |(_, title)| title);
        notify_rust::Notification::new()
            .summary(if title.is_empty() { "zawk" } else { title })
            .body(body)
            .show()
            .map(drop)
            .map_err(|e| format!("failed to show notification: {}", e))
    } else {
        Err(format!(
            "unsupported scheme {:?}, expected nats://, nats+tls://, kafka://, mqtt://, mqtts:// or desktop://",
            scheme,
        ))
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
                $client
                    .publish(topic, $qos, false, $payload)
                    .map_err(|e| format!("failed to publish: {}", e))?;
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
                publish_and_flush!(
                    client,
                    conn,
                    rumqttc::QoS::AtMostOnce,
                    rumqttc::Event::Outgoing,
                    body.as_bytes().to_vec()
                )
            }
            MqttClient::V5(client, conn) => {
                publish_and_flush!(
                    client,
                    conn,
                    rumqttc::v5::mqttbytes::QoS::AtMostOnce,
                    rumqttc::v5::Event::Outgoing,
                    body.to_string()
                )
            }
        }
    }
}

fn mqtt_connect(url: &Url) -> std::result::Result<MqttClient, String> {
    let tls = url.scheme() == "mqtts";
    let host = match url
        .host()
        .ok_or_else(|| format!("missing host in {:?}", redact_url(url.as_str())))?
    {
        url::Host::Domain(domain) => domain.to_string(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    let port = url.port().unwrap_or(if tls { 8883 } else { 1883 });
    // MQTT 3.1.1 limits client ids to 23 characters
    let client_id = format!("zawk-{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
    // username + password, or a token (JWT style) as the password only; userinfo is percent-encoded in the URL
    let decode = |s: &str| {
        urlencoding::decode(s)
            .map(|s| s.into_owned())
            .unwrap_or_else(|_| s.to_string())
    };
    let user_name = url.username();
    let credentials = if user_name.is_empty() {
        None
    } else if let Some(password) = url.password() {
        Some((decode(user_name), decode(password)))
    } else {
        Some((String::new(), decode(user_name)))
    };
    let is_v5 = !url
        .query_pairs()
        .any(|(key, version)| key == "version" && version.contains("3.1"));
    let transport = if tls {
        rumqttc::Transport::tls_with_config(crate::runtime::tls::client_config()?.into())
    } else {
        rumqttc::Transport::tcp()
    };
    let keep_alive = Duration::from_secs(60);
    if is_v5 {
        let mut options = rumqttc::v5::MqttOptions::new(client_id, host, port);
        options
            .set_keep_alive(keep_alive)
            .set_clean_start(true)
            .set_transport(transport);
        if let Some((user_name, password)) = credentials {
            options.set_credentials(user_name, password);
        }
        let (client, conn) = rumqttc::v5::Client::new(options, 10);
        Ok(MqttClient::V5(client, conn))
    } else {
        let mut options = rumqttc::MqttOptions::new(client_id, host, port);
        options
            .set_keep_alive(keep_alive)
            .set_clean_session(true)
            .set_transport(transport);
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

fn try_send_mail(
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
    smtp_url: &str,
) -> std::result::Result<(), String> {
    let smtp_url = if smtp_url.is_empty() {
        env::var("SMTP_URL").unwrap_or_default()
    } else {
        smtp_url.to_owned()
    };
    let (api_url, api_key) = if !smtp_url.is_empty() {
        (smtp_url, String::new())
    } else if let Ok(api_key) = env::var("MLSN_API_KEY") {
        ("https://api.mailersend.com/v1/email".to_owned(), api_key)
    } else if let Ok(api_key) = env::var("RESEND_API_KEY") {
        ("https://api.resend.com/emails".to_owned(), api_key)
    } else {
        return Err(
            "no mail transport: pass smtp_url or set SMTP_URL, MLSN_API_KEY or RESEND_API_KEY"
                .to_owned(),
        );
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
    let mut builder = HTTP_CLIENT
        .post(&api_url)
        .header("Authorization", format!("Bearer {}", api_key));
    if api_url.starts_with("https://api.resend.com") {
        let receivers: Vec<String> = to.split(',').map(|s| s.trim().to_string()).collect();
        let req = ResendRequest {
            from: from.to_string(),
            to: receivers,
            subject: subject.to_string(),
            text: text.to_string(),
        };
        builder = builder.json(&req);
    } else {
        let receivers = to
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|email| MailAddress::new(email.trim()))
            .collect();
        let req = MailerSendRequest {
            from: MailAddress::new(from),
            to: receivers,
            subject: subject.to_string(),
            text: text.to_string(),
        };
        builder = builder.json(&req);
    }
    let resp = builder
        .send()
        .map_err(|e| format!("failed to send mail via {}: {}", redact_url(&api_url), e))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .unwrap_or_else(|e| format!("(failed to read response body: {})", e));
        return Err(format!(
            "failed to send mail via {}: HTTP {} {}",
            redact_url(&api_url),
            status,
            body
        ));
    }
    Ok(())
}

fn try_smtp_send(
    url: &str,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> std::result::Result<(), String> {
    let address = |text: &str| {
        text.trim()
            .parse::<lettre::message::Mailbox>()
            .map_err(|e| format!("invalid mail address {:?}: {}", text, e))
    };
    let mut builder = lettre::Message::builder()
        .from(address(from)?)
        .subject(subject);
    for email_address in to.split(",") {
        if email_address.contains('@') {
            builder = builder.to(address(email_address.trim())?);
        }
    }
    let email = builder
        .header(lettre::message::header::ContentType::TEXT_PLAIN)
        .body(String::from(text))
        .map_err(|e| e.to_string())?;
    let mailer = lettre::SmtpTransport::from_url(url)
        .map_err(|e| format!("invalid SMTP URL: {}", e))?
        .build();
    mailer
        .send(&email)
        .map(drop)
        .map_err(|e| format!("failed to send mail: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use local_ip_address::local_ip;

    #[test]
    fn test_merge_headers() {
        let mut headers = HeaderMap::new();
        headers.append(
            "set-cookie",
            "a=1; Expires=Wed, 21 Oct 2026 07:28:00 GMT"
                .parse()
                .unwrap(),
        );
        headers.append("set-cookie", "b=2".parse().unwrap());
        headers.append("vary", "Accept".parse().unwrap());
        headers.append("vary", "Origin".parse().unwrap());
        headers.append("content-type", "text/plain".parse().unwrap());
        let merged: HashMap<String, String> = merge_headers(&headers).into_iter().collect();
        assert_eq!(
            merged["set-cookie"],
            "a=1; Expires=Wed, 21 Oct 2026 07:28:00 GMT\nb=2"
        );
        assert_eq!(merged["vary"], "Accept, Origin");
        assert_eq!(merged["content-type"], "text/plain");
    }

    #[test]
    fn test_redact_url() {
        assert_eq!(
            redact_url("redis://user:s3cret@127.0.0.1:1/h"),
            "redis://user:***@127.0.0.1:1/h"
        );
        assert_eq!(
            redact_url("redis://:s3cret@127.0.0.1"),
            "redis://:***@127.0.0.1"
        );
        assert_eq!(
            redact_url("nats://token@127.0.0.1:4222"),
            "nats://***@127.0.0.1:4222"
        );
        assert_eq!(
            redact_url("smtps://u%40x.com:p@ss@smtp.x.com:465"),
            "smtps://u%40x.com:***@smtp.x.com:465"
        );
        assert_eq!(
            redact_url("http://127.0.0.1/a@b?x=@"),
            "http://127.0.0.1/a@b?x=@"
        );
        assert_eq!(redact_url("127.0.0.1:4222"), "127.0.0.1:4222");
        // raw passwords with reserved characters
        assert_eq!(
            redact_url("mysql://root:p/ss@127.0.0.1:3306/test#people"),
            "mysql://root:***@127.0.0.1:3306/test#people"
        );
        assert_eq!(
            redact_url("mysql://root:p?ss@127.0.0.1:3306/test#people"),
            "mysql://root:***@127.0.0.1:3306/test#people"
        );
        assert_eq!(
            redact_url("mysql://root:p#ss@127.0.0.1:3306/test#people"),
            "mysql://root:***@127.0.0.1:3306/test#people"
        );
        assert_eq!(
            redact_url("redis://u:a/b?c#d@e@h:6379/0"),
            "redis://u:***@h:6379/0"
        );
        assert_eq!(
            redact_url("mysql://u:p#w@h/db#select * from t where e = 'a@b'"),
            "mysql://u:***@h/db#select * from t where e = 'a@b'"
        );
        assert_eq!(
            redact_url("postgres://u:p/w@h/db?password=x#t"),
            "postgres://u:***@h/db?password=***#t"
        );
        // an `@` in a path or a query is not a userinfo
        assert_eq!(
            redact_url("https://registry.npmjs.org/@types/node"),
            "https://registry.npmjs.org/@types/node"
        );
        assert_eq!(
            redact_url("http://h/x?email=a@b.com"),
            "http://h/x?email=a@b.com"
        );
        assert_eq!(
            redact_url("postgres://127.0.0.1:1/db?user=u&password=secret#people"),
            "postgres://127.0.0.1:1/db?user=u&password=***#people"
        );
        assert_eq!(
            redact_url("postgres://u:p@h/db?sslmode=disable&PWD=x&token=t&secret=#t"),
            "postgres://u:***@h/db?sslmode=disable&PWD=***&token=***&secret=#t"
        );
        assert_eq!(
            redact_url("http://h/x?passwords=a&key=b"),
            "http://h/x?passwords=a&key=b"
        );
        assert_eq!(
            redact_url("mysql://root:123456@127.0.0.1:3306/test#people"),
            "mysql://root:***@127.0.0.1:3306/test#people"
        );
        assert_eq!(
            redact_url("sqlite://./a.db#select * from t where e = 'a@b' and q = '?password=1'"),
            "sqlite://./a.db#select * from t where e = 'a@b' and q = '?password=1'"
        );
    }

    #[test]
    fn test_local_ip() {
        let my_local_ip = local_ip().unwrap();
        println!("This is my local IP address: {:?}", my_local_ip);
    }

    #[test]
    fn test_http_get() {
        let url = "http://127.0.0.1:18080/ip";
        let headers: StrMap<Str> = StrMap::default();
        let resp = http_get(url, &headers);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    fn test_http_post() {
        let url = "http://127.0.0.1:18080/post";
        let headers: StrMap<Str> = StrMap::default();
        let body = Str::from(r#"{"status": "ok"}"#);
        let resp = http_post(url, &headers, &body);
        println!("{}", resp.get(&Str::from("text")));
    }

    #[test]
    fn test_publish_unknown_scheme() {
        assert!(
            try_publish("redis://127.0.0.1:6379/x", "hello")
                .unwrap_err()
                .contains("unsupported scheme \"redis\"")
        );
        assert!(
            try_publish("nats//typo", "hello")
                .unwrap_err()
                .contains("invalid URL")
        );
        assert!(
            try_publish("notification", "hello")
                .unwrap_err()
                .contains("invalid URL")
        );
        let err = try_publish("ftp://user:secret@host/x", "hello").unwrap_err();
        assert!(!err.contains("secret"));
    }

    #[test]
    fn test_publish_nats() {
        let url = "nats://127.0.0.1:14222/topic1";
        publish(url, "Hello World!");
    }

    /// A server that accepts one connection, answers until the first message, then goes away for good.
    fn nats_server_closing_after_first_message() -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            drop(listener);
            stream
                .write_all(b"INFO {\"server_id\":\"mock\",\"server_name\":\"mock\",\"version\":\"2.10.0\",\"proto\":1,\"host\":\"127.0.0.1\",\"port\":4222,\"headers\":true,\"max_payload\":1048576}\r\n")
                .unwrap();
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = stream.read(&mut buf) {
                let request = String::from_utf8_lossy(&buf[..n]);
                if request.contains("PING") {
                    stream.write_all(b"PONG\r\n").unwrap();
                }
                if request.contains("PUB ") {
                    break;
                }
            }
        });
        port
    }

    #[test]
    fn test_publish_nats_times_out_when_server_is_gone() {
        let url = format!(
            "nats://127.0.0.1:{}/topic1",
            nats_server_closing_after_first_message()
        );
        try_publish(&url, "first").unwrap();
        // let the client see the closed connection: it now reconnects forever
        std::thread::sleep(Duration::from_millis(200));
        assert!(try_publish(&url, "second").unwrap_err().contains("timed out"));
    }

    /// A server that accepts connections and never answers, so that a client waits for its timeout.
    /// Sends one message on the channel for each connection accepted.
    fn silent_server() -> (u16, std::sync::mpsc::Receiver<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted_tx, accepted_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut streams = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                streams.push(stream);
                let _ = accepted_tx.send(());
            }
        });
        (port, accepted_rx)
    }

    #[test]
    fn test_nats_connect_does_not_block_other_urls() {
        let (silent_port, accepted) = silent_server();
        std::thread::spawn(move || nats_client(&format!("nats://127.0.0.1:{}", silent_port)));
        accepted.recv_timeout(Duration::from_secs(5)).unwrap();
        // the connection above is still waiting for the server: this one must not wait for it
        let url = format!(
            "nats://127.0.0.1:{}",
            nats_server_closing_after_first_message()
        );
        let start = std::time::Instant::now();
        nats_client(&url).unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
    }

    /// An MQTT 3.1.1 broker that accepts connections and takes any message.
    fn mqtt_broker() -> u16 {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let mut connected = false;
                    while let Ok(1..) = stream.read(&mut buf) {
                        if !connected {
                            connected = true;
                            // CONNACK: session not present, connection accepted
                            stream.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap();
                        }
                    }
                });
            }
        });
        port
    }

    #[test]
    fn test_mqtt_publish_does_not_block_other_urls() {
        let (silent_port, accepted) = silent_server();
        let silent = format!("mqtt://127.0.0.1:{}/t?version=3.1.1", silent_port);
        std::thread::spawn(move || try_publish(&silent, "lost"));
        accepted.recv_timeout(Duration::from_secs(5)).unwrap();
        // the publish above is still waiting for the broker: this one must not wait for it
        let url = format!("mqtt://127.0.0.1:{}/t?version=3.1.1", mqtt_broker());
        let start = std::time::Instant::now();
        try_publish(&url, "hello").unwrap();
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        // the connection went back to the pool, and is used again
        try_publish(&url, "hello again").unwrap();
    }

    #[test]
    fn test_publish_mqtt() {
        let url = "mqtt://127.0.0.1:1883/topic1";
        publish(url, "Hello World!");
    }

    #[test]
    fn test_send_email() {
        dotenvy::dotenv().ok();
        let from = "support@trial-3zxk54v3ykzgjy6v.mlsender.net";
        let to = "linux_china@hotmail.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        send_mail(from, to, subject, text, "");
    }

    #[test]
    fn test_send_smtp() {
        dotenvy::dotenv().ok();
        let smtp_url = env::var("SMTP_URL").unwrap();
        println!("SMTP_URL: {}", smtp_url);
        let from = "libing.chen@example";
        let to = "linux_china@example.com";
        let subject = "demo.csv processed successfully by zawk";
        let text = "rows: 180, total: 1000";
        send_mail(from, to, subject, text, &smtp_url);
    }
}
