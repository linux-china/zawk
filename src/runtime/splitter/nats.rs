//! NATS subject input: `zawk 'MSG_SUBJECT ~ /demo/ { print }' 'nats://localhost:4222/logs.>'`.
//!
//! The subject may contain wildcards (`orders.*.created`, `orders.>`), see [`super::message`] for
//! how the messages are read.
//!
//! `?queue=name` subscribes as a member of the queue group `name`: each message goes to one member.

use std::sync::atomic::Ordering;
use std::time::Duration;

use futures_util::StreamExt;
use url::Url;

use super::message::{catch_sigint, MessageSource, Received, INTERRUPTED, WAITING};
use crate::runtime::network::{nats_block_on, nats_client, redact_url, NATS_RUNTIME};

/// Whether `path` is a NATS subject to read messages from.
pub fn is_nats_url(path: &str) -> bool {
    path.starts_with("nats://") || path.starts_with("nats+tls://")
}

/// A subscription to the subject of a `nats://[user:password@]host[:port]/subject[?queue=name]` URL.
pub struct Subscription {
    // Only None once dropped.
    subscriber: Option<async_nats::Subscriber>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        // dropping a subscriber unsubscribes on the runtime of the client
        let _guard = NATS_RUNTIME.enter();
        drop(self.subscriber.take());
    }
}

impl Subscription {
    pub fn new(url: &str) -> std::result::Result<Subscription, String> {
        let parsed = Url::parse(url).map_err(|e| format!("invalid URL {:?}: {}", redact_url(url), e))?;
        if parsed.host().is_none() {
            return Err(format!("missing host in {:?}", redact_url(url)));
        }
        // `>` (and spaces) are percent-encoded in the path of a URL
        let path = parsed.path().strip_prefix('/').unwrap_or(parsed.path());
        let subject = urlencoding::decode(path).map_err(|e| e.to_string())?.into_owned();
        if subject.is_empty() {
            return Err(format!("missing subject in {:?}", redact_url(url)));
        }
        let mut queue = None;
        for (name, value) in parsed.query_pairs() {
            match &*name {
                "queue" if !value.is_empty() => queue = Some(value.into_owned()),
                _ => return Err(format!("unsupported query parameter {:?} in {:?}", name, redact_url(url))),
            }
        }
        let conn_url = parsed[..url::Position::BeforePath].replacen("nats+tls://", "tls://", 1);
        let client = nats_client(&conn_url)?;
        let subscriber = nats_block_on(async {
            let subscriber = match queue {
                Some(queue) => client.queue_subscribe(subject.clone(), queue).await,
                None => client.subscribe(subject.clone()).await,
            }
                .map_err(|e| format!("failed to subscribe to {:?}: {}", subject, e))?;
            // the server knows about the subscription once the SUB is flushed
            client.flush().await.map_err(|e| format!("failed to subscribe to {:?}: {}", subject, e))?;
            Ok::<_, String>(subscriber)
        })?;
        catch_sigint();
        Ok(Subscription { subscriber: Some(subscriber) })
    }

    fn next_message(&mut self) -> Option<async_nats::Message> {
        loop {
            if INTERRUPTED.load(Ordering::SeqCst) {
                return None;
            }
            let subscriber = self.subscriber.as_mut()?;
            WAITING.store(true, Ordering::SeqCst);
            let next = NATS_RUNTIME
                .block_on(async { tokio::time::timeout(Duration::from_millis(100), subscriber.next()).await });
            WAITING.store(false, Ordering::SeqCst);
            match next {
                Ok(Some(msg)) => return Some(msg),
                Ok(None) => {
                    eprintln_ignore!("zawk: warning: NATS subscription closed");
                    return None;
                }
                Err(_elapsed) => continue,
            }
        }
    }
}

impl MessageSource for Subscription {
    fn service(&self) -> &'static str {
        "NATS"
    }

    fn next(&mut self) -> Option<Received> {
        let msg = self.next_message()?;
        let headers = msg
            .headers
            .as_ref()
            .map(|headers| {
                headers
                    .iter()
                    .map(|(name, values)| {
                        let values: Vec<String> = values.iter().map(|v| v.to_string()).collect();
                        (name.to_string(), values.join(", "))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(Received { subject: msg.subject.to_string(), headers, payload: msg.payload.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::splitter::message::{chunk_size, current_message, payload_reader, MessageReader};
    use crate::runtime::splitter::LineReader;
    use crate::runtime::{RegexCache, Str};

    #[test]
    fn test_is_nats_url() {
        assert!(is_nats_url("nats://127.0.0.1:14222/logs.>"));
        assert!(is_nats_url("nats+tls://host/orders.*.created"));
        assert!(!is_nats_url("demo.txt"));
    }

    #[test]
    fn test_missing_subject() {
        assert!(Subscription::new("nats://127.0.0.1:14222").err().unwrap().contains("missing subject"));
        assert!(Subscription::new("nats://127.0.0.1:14222/").err().unwrap().contains("missing subject"));
    }

    #[test]
    fn test_unsupported_query() {
        let err = Subscription::new("nats://127.0.0.1:14222/logs?group=a").err().unwrap();
        assert!(err.contains("unsupported query parameter \"group\""), "{}", err);
        let err = Subscription::new("nats://127.0.0.1:14222/logs?queue=").err().unwrap();
        assert!(err.contains("unsupported query parameter \"queue\""), "{}", err);
    }

    #[test]
    fn test_read_messages() {
        use crate::runtime::splitter::regex::RegexSplitter;
        let subscription = Subscription::new("nats://127.0.0.1:14222/zawk.test.%3E").unwrap();
        let mut reader = MessageReader::new(Box::new(subscription), false, false, |payload, subject| {
            RegexSplitter::new(payload_reader(payload), chunk_size(1 << 20, 0), subject, false)
        });
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.a", "a1\na2");
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.empty", "");
        crate::runtime::network::publish("nats://127.0.0.1:14222/zawk.test.b.c", "b1");
        let mut rc = RegexCache::default();
        let mut records = Vec::new();
        for _ in 0..3 {
            let (changed, line) = reader.read_line(&Str::from("\n"), &mut rc).unwrap();
            records.push((changed, reader.filename().to_string(), line.line.to_string()));
            assert_eq!(current_message().unwrap().subject, reader.filename().to_string());
        }
        assert_eq!(
            records,
            [
                (true, "zawk.test.a".to_string(), "a1".to_string()),
                (false, "zawk.test.a".to_string(), "a2".to_string()),
                (true, "zawk.test.b.c".to_string(), "b1".to_string()),
            ]
        );
    }
}
