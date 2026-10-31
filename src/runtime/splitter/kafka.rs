//! Kafka topic input: `zawk '{ print $1 }' 'kafka://localhost:9092/topic1'`.
//!
//! See [`super::message`] for how the messages are read. MSG_SUBJECT and FILENAME are the topic.
//!
//! `?group=name` joins the consumer group `name`: the partitions of the topic are shared by its
//! members and the offsets are committed, so a later run resumes where this one stopped. Without
//! it, zawk joins a group of its own that is not used again.
//!
//! `?offset=earliest` starts at the oldest message when the group has no committed offset,
//! `?offset=latest` (the default) only reads the messages sent from now on.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use confluent_kafka::common::header::{Header, Headers};
use confluent_kafka::common::serialization::ByteArrayDeserializer;
use confluent_kafka::consumer::{
    CloseOptions, Consumer, ConsumerConfig, GroupProtocol, KafkaConsumer,
};
use url::Url;
use uuid::Uuid;

use super::message::{INTERRUPTED, MessageSource, Received, WAITING, catch_sigint};
use crate::runtime::network::{KAFKA_RUNTIME, is_power_of_ten, redact_url, url_topic};

const DEFAULT_PORT: u16 = 9092;
/// How long to wait for the broker, at start and when leaving the group.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How often SIGINT is checked while waiting for a message.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long the input is idle before checking that the broker still answers: `poll` returns no
/// error when the connection is lost, it only finds no records.
const PROBE_INTERVAL: Duration = Duration::from_secs(10);
/// How long a check waits for the broker. SIGINT is not seen meanwhile.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether `path` is a Kafka topic to read messages from.
pub fn is_kafka_url(path: &str) -> bool {
    path.starts_with("kafka://")
}

/// The bootstrap server (`host:port`) and the topic of a `kafka://host[:port]/topic` URL.
pub(crate) fn endpoint(parsed: &Url, url: &str) -> Result<(String, String), String> {
    let Some(host) = parsed.host_str() else {
        return Err(format!("missing host in {:?}", redact_url(url)));
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(format!(
            "user and password are not supported in {:?}",
            redact_url(url)
        ));
    }
    let topic = url_topic(parsed)?;
    if topic.is_empty() {
        return Err(format!("missing topic in {:?}", redact_url(url)));
    }
    Ok((
        format!("{}:{}", host, parsed.port().unwrap_or(DEFAULT_PORT)),
        topic,
    ))
}

/// The record key of a `kafka://host[:port]/topic[?key=value]` URL given to `publish`.
pub(crate) fn publish_key(parsed: &Url, url: &str) -> Result<Option<String>, String> {
    let mut key = None;
    for (name, value) in parsed.query_pairs() {
        match &*name {
            "key" if !value.is_empty() => key = Some(value.into_owned()),
            _ => {
                return Err(format!(
                    "unsupported query parameter {:?} in {:?}",
                    name,
                    redact_url(url)
                ));
            }
        }
    }
    Ok(key)
}

/// What a `kafka://host[:port]/topic[?group=name][&offset=earliest|latest]` URL asks for.
#[derive(Debug, PartialEq)]
struct Target {
    bootstrap_servers: String,
    topic: String,
    group: Option<String>,
    offset: String,
}

impl Target {
    fn parse(url: &str) -> Result<Target, String> {
        let parsed =
            Url::parse(url).map_err(|e| format!("invalid URL {:?}: {}", redact_url(url), e))?;
        let (bootstrap_servers, topic) = endpoint(&parsed, url)?;
        let mut group = None;
        let mut offset = "latest".to_string();
        for (name, value) in parsed.query_pairs() {
            match &*name {
                "group" if !value.is_empty() => group = Some(value.into_owned()),
                "offset" if value == "earliest" || value == "latest" => offset = value.into_owned(),
                _ => {
                    return Err(format!(
                        "unsupported query parameter {:?} in {:?}",
                        name,
                        redact_url(url)
                    ));
                }
            }
        }
        Ok(Target {
            bootstrap_servers,
            topic,
            group,
            offset,
        })
    }
}

/// A member of a consumer group subscribed to a topic.
pub struct Subscription {
    // Only None once closed.
    consumer: Option<KafkaConsumer<Vec<u8>, Vec<u8>>>,
    bootstrap_servers: String,
    next_probe: Instant,
    health: BrokerHealth,
}

/// The failures seen since the broker last answered.
#[derive(Debug, Default)]
struct BrokerHealth {
    failures: u64,
}

impl BrokerHealth {
    /// Count a failure. Whether to warn about it.
    fn failed(&mut self) -> bool {
        self.failures += 1;
        is_power_of_ten(self.failures)
    }

    /// The broker answered. Whether it did not before.
    fn recovered(&mut self) -> bool {
        std::mem::take(&mut self.failures) > 0
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.close();
    }
}

impl Subscription {
    pub fn new(url: &str) -> Result<Subscription, String> {
        let Target {
            bootstrap_servers,
            topic,
            group,
            offset,
        } = Target::parse(url)?;
        let group = group.unwrap_or_else(|| format!("zawk-{}", Uuid::new_v4()));
        let props = HashMap::from([
            (
                ConsumerConfig::BOOTSTRAP_SERVERS_CONFIG.to_string(),
                bootstrap_servers.clone(),
            ),
            (ConsumerConfig::GROUP_ID_CONFIG.to_string(), group),
            (
                ConsumerConfig::GROUP_PROTOCOL_CONFIG.to_string(),
                GroupProtocol::Consumer.to_string(),
            ),
            (ConsumerConfig::AUTO_OFFSET_RESET_CONFIG.to_string(), offset),
            // A record is committed once it is returned: one at a time, the records left when
            // SIGINT ends the input are read again by the next run of the group.
            (
                ConsumerConfig::MAX_POLL_RECORDS_CONFIG.to_string(),
                "1".to_string(),
            ),
        ]);
        let consumer = KAFKA_RUNTIME.block_on(async {
            let config = ConsumerConfig::new(&props).map_err(|e| e.to_string())?;
            let mut consumer = KafkaConsumer::new(
                config,
                ByteArrayDeserializer::new(),
                ByteArrayDeserializer::new(),
            )
            .map_err(|e| e.to_string())?;
            // the client retries for ever: tell now if the broker cannot be reached
            consumer
                .list_topics_with_timeout(TIMEOUT)
                .await
                .map_err(|e| format!("failed to connect to {}: {}", bootstrap_servers, e))?;
            consumer
                .subscribe_with_topics(vec![topic.clone()])
                .await
                .map_err(|e| format!("failed to subscribe to {:?}: {}", topic, e))?;
            Ok::<_, String>(consumer)
        })?;
        catch_sigint();
        Ok(Subscription {
            consumer: Some(consumer),
            bootstrap_servers,
            next_probe: Instant::now() + PROBE_INTERVAL,
            health: BrokerHealth::default(),
        })
    }

    fn broker_answered(&mut self) {
        self.next_probe = Instant::now() + PROBE_INTERVAL;
        if self.health.recovered() {
            eprintln_ignore!(
                "zawk: Kafka broker {} is answering again",
                self.bootstrap_servers
            );
        }
    }

    fn broker_failed(&mut self, error: &dyn std::fmt::Display) {
        self.next_probe = Instant::now() + PROBE_INTERVAL;
        if self.health.failed() {
            eprintln_ignore!(
                "zawk: warning: Kafka broker {} is not answering (failures so far: {}), still retrying: {}",
                self.bootstrap_servers,
                self.health.failures,
                error
            );
        }
    }

    /// Tell an input without messages from a broker that is gone.
    fn probe_broker(&mut self) {
        let Some(consumer) = self.consumer.as_mut() else {
            return;
        };
        WAITING.store(true, Ordering::SeqCst);
        let answer = KAFKA_RUNTIME.block_on(consumer.list_topics_with_timeout(PROBE_TIMEOUT));
        WAITING.store(false, Ordering::SeqCst);
        match answer {
            Ok(_) => self.broker_answered(),
            Err(e) => self.broker_failed(&e),
        }
    }

    /// Commit the offsets and leave the group, without waiting for the session to time out.
    fn close(&mut self) {
        let Some(mut consumer) = self.consumer.take() else {
            return;
        };
        let closed = KAFKA_RUNTIME
            .block_on(consumer.close_with_options(CloseOptions::new_with_timeout(TIMEOUT)));
        if let Err(e) = closed {
            eprintln_ignore!("zawk: warning: failed to close Kafka consumer: {}", e);
        }
    }
}

impl MessageSource for Subscription {
    fn service(&self) -> &'static str {
        "Kafka"
    }

    fn next(&mut self) -> Option<Received> {
        loop {
            if INTERRUPTED.load(Ordering::SeqCst) {
                self.close();
                return None;
            }
            let consumer = self.consumer.as_mut()?;
            WAITING.store(true, Ordering::SeqCst);
            let polled = KAFKA_RUNTIME.block_on(consumer.poll(POLL_INTERVAL));
            WAITING.store(false, Ordering::SeqCst);
            match polled {
                Ok(records) => {
                    if let Some(record) = records.into_iter().next() {
                        self.broker_answered();
                        let mut headers: Vec<(String, String)> = Vec::new();
                        for header in record.headers().iter() {
                            let value = String::from_utf8_lossy(header.value().unwrap_or_default());
                            match headers.iter_mut().find(|(name, _)| name == header.key()) {
                                Some((_, values)) => {
                                    values.push_str(", ");
                                    values.push_str(&value);
                                }
                                None => {
                                    headers.push((header.key().to_string(), value.into_owned()))
                                }
                            }
                        }
                        // a tombstone has no value: an empty payload, without records
                        let payload = record.value().cloned().unwrap_or_default();
                        return Some(Received {
                            subject: record.topic().to_string(),
                            headers,
                            payload,
                        });
                    }
                    if Instant::now() >= self.next_probe {
                        self.probe_broker();
                    }
                }
                Err(e) if e.is_retriable_error() => {
                    self.broker_failed(&e);
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(e) => {
                    eprintln_ignore!("zawk: warning: Kafka subscription closed: {}", e);
                    self.close();
                    return None;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use confluent_kafka::common::header::{RecordHeader, RecordHeaders};
    use confluent_kafka::common::serialization::StringSerializer;
    use confluent_kafka::producer::{KafkaProducer, Producer, ProducerConfig, ProducerRecord};

    use super::*;
    use crate::runtime::splitter::LineReader;
    use crate::runtime::splitter::message::{
        MessageReader, chunk_size, current_message, payload_reader,
    };
    use crate::runtime::splitter::regex::RegexSplitter;
    use crate::runtime::{RegexCache, Str};

    #[test]
    fn test_is_kafka_url() {
        assert!(is_kafka_url("kafka://localhost:9092/topic1"));
        assert!(!is_kafka_url("nats://127.0.0.1:14222/topic1"));
        assert!(!is_kafka_url("demo.txt"));
    }

    #[test]
    fn test_broker_health() {
        let mut health = BrokerHealth::default();
        assert!(!health.recovered());
        let warned: Vec<usize> = (1..=100).filter(|_| health.failed()).collect();
        assert_eq!(warned, [1, 10, 100]);
        assert!(health.recovered());
        assert!(!health.recovered());
        assert!(health.failed());
    }

    #[test]
    fn test_parse() {
        assert_eq!(
            Target::parse("kafka://broker/logs").unwrap(),
            Target {
                bootstrap_servers: "broker:9092".to_string(),
                topic: "logs".to_string(),
                group: None,
                offset: "latest".to_string(),
            }
        );
        assert_eq!(
            Target::parse("kafka://localhost:9092/logs?group=g1&offset=earliest").unwrap(),
            Target {
                bootstrap_servers: "localhost:9092".to_string(),
                topic: "logs".to_string(),
                group: Some("g1".to_string()),
                offset: "earliest".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_errors() {
        let error = |url| Target::parse(url).unwrap_err();
        assert!(error("kafka://localhost:9092").contains("missing topic"));
        assert!(error("kafka://localhost:9092/").contains("missing topic"));
        assert!(error("kafka://user:secret@localhost:9092/logs").contains("not supported"));
        assert!(!error("kafka://user:secret@localhost:9092/logs").contains("secret"));
        assert!(
            error("kafka://host/logs?queue=a").contains("unsupported query parameter \"queue\"")
        );
        assert!(
            error("kafka://host/logs?group=").contains("unsupported query parameter \"group\"")
        );
        assert!(
            error("kafka://host/logs?offset=none")
                .contains("unsupported query parameter \"offset\"")
        );
    }

    #[test]
    fn test_publish_key() {
        let key = |url: &str| publish_key(&Url::parse(url).unwrap(), url);
        assert_eq!(key("kafka://broker/logs"), Ok(None));
        assert_eq!(
            key("kafka://broker/logs?key=user1"),
            Ok(Some("user1".to_string()))
        );
        assert_eq!(
            key("kafka://broker/logs?key=a%26b%20c"),
            Ok(Some("a&b c".to_string()))
        );
        assert!(
            key("kafka://broker/logs?key=")
                .unwrap_err()
                .contains("unsupported query parameter \"key\"")
        );
        assert!(
            key("kafka://broker/logs?group=g1")
                .unwrap_err()
                .contains("unsupported query parameter \"group\"")
        );
        assert!(
            key("kafka://broker/logs?key=a&acks=1")
                .unwrap_err()
                .contains("unsupported query parameter \"acks\"")
        );
    }

    fn send(topic: &str, records: &[(&str, &[(&str, &str)])]) {
        let props = HashMap::from([(
            "bootstrap.servers".to_string(),
            "localhost:9092".to_string(),
        )]);
        KAFKA_RUNTIME.block_on(async {
            let config = ProducerConfig::new(&props).unwrap();
            let producer = KafkaProducer::<String, String>::new(
                config,
                StringSerializer::default(),
                StringSerializer::default(),
            )
            .unwrap();
            for (value, headers) in records {
                let headers = headers
                    .iter()
                    .map(|(k, v)| RecordHeader::new(k.to_string(), Some(v.as_bytes().to_vec())));
                let record = ProducerRecord::with_partition_key_headers(
                    topic.to_string(),
                    None,
                    None,
                    Some(value.to_string()),
                    RecordHeaders::with_header_iter(headers),
                )
                .unwrap();
                producer.send(record).await.unwrap();
            }
            producer.close().await.unwrap();
        });
    }

    #[test]
    fn test_publish() {
        let topic = format!("zawk-test-{}", Uuid::new_v4());
        let url = format!("kafka://localhost:9092/{}", topic);
        crate::runtime::network::publish(&url, "one");
        crate::runtime::network::publish(&url, "two");
        let mut subscription = Subscription::new(&format!("{}?offset=earliest", url)).unwrap();
        let payloads: Vec<Vec<u8>> = (0..2)
            .map(|_| subscription.next().unwrap().payload)
            .collect();
        assert_eq!(payloads, [b"one".to_vec(), b"two".to_vec()]);
    }

    #[test]
    fn test_publish_with_key() {
        let topic = format!("zawk-test-{}", Uuid::new_v4());
        let url = format!("kafka://localhost:9092/{}", topic);
        crate::runtime::network::publish(&format!("{}?key=user%201", url), "one");
        crate::runtime::network::publish(&url, "two");
        let mut subscription = Subscription::new(&format!("{}?offset=earliest", url)).unwrap();
        let consumer = subscription.consumer.as_mut().unwrap();
        let mut keys = Vec::new();
        while keys.len() < 2 {
            let records = KAFKA_RUNTIME
                .block_on(consumer.poll(POLL_INTERVAL))
                .unwrap();
            keys.extend(records.into_iter().map(|record| record.key().cloned()));
        }
        assert_eq!(keys, [Some(b"user 1".to_vec()), None]);
    }

    #[test]
    fn test_read_messages() {
        let topic = format!("zawk-test-{}", Uuid::new_v4());
        send(
            &topic,
            &[
                ("a1\na2", &[("h", "1"), ("h", "2"), ("k", "v")]),
                ("", &[]),
                ("b1", &[]),
            ],
        );
        let url = format!("kafka://localhost:9092/{}?offset=earliest", topic);
        let subscription = Subscription::new(&url).unwrap();
        let mut reader =
            MessageReader::new(Box::new(subscription), false, false, |payload, subject| {
                RegexSplitter::new(
                    payload_reader(payload),
                    chunk_size(1 << 20, 0),
                    subject,
                    false,
                )
            });
        let mut rc = RegexCache::default();
        let mut records = Vec::new();
        let mut headers = Vec::new();
        for _ in 0..3 {
            let (changed, line) = reader.read_line(&Str::from("\n"), &mut rc).unwrap();
            records.push((changed, line.line.to_string()));
            let message = current_message().unwrap();
            assert_eq!(message.subject, reader.filename().to_string());
            assert_eq!(message.subject, topic);
            if records.len() == 1 {
                headers = message.headers.clone();
            }
        }
        assert_eq!(
            records,
            [
                (true, "a1".to_string()),
                (false, "a2".to_string()),
                (true, "b1".to_string())
            ]
        );
        assert_eq!(
            headers,
            [
                ("h".to_string(), "1, 2".to_string()),
                ("k".to_string(), "v".to_string())
            ]
        );
    }
}
