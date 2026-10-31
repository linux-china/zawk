---
title: Streaming with Kafka
sidebar_label: Kafka
sidebar_position: 3
---

# Streaming with Kafka

With zawk, the stream can be a [Kafka](https://kafka.apache.org/) topic. A ten-line AWK script becomes a
consumer: no JVM, no project, one binary.

## Subscribe: a Kafka topic as input

Pass a `kafka://` URL as the only input file:

```shell
zawk '{ print $1 }' 'kafka://localhost:9092/topic1'
```

- The URL is `kafka://host[:port]/topic`; the port defaults to `9092`.
- zawk reads messages until it receives **Ctrl+C** (SIGINT), then runs `END` and exits. The output of each
  message is written at once. A second Ctrl+C exits immediately (status 130) without running `END`.
- `?group=name` joins the consumer group `name`: several zawk workers share the partitions of the topic, and a
  later run resumes where the group stopped: `kafka://localhost:9092/orders?group=workers`.
- `?offset=earliest` starts at the oldest message when the group has no committed offset. The default,
  `?offset=latest`, only reads the messages sent from now on.

Without `?group=`, zawk joins a new group of its own, so every run starts according to `?offset=`.

Each message is read as an **input file of its own**: `FNR` restarts at 1 and `FILENAME` is the topic.
These variables describe the message:

| Variable | Value |
|----------|-------|
| `MSG_SUBJECT` | the topic, e.g. `orders` |
| `MSG_HEADERS` | the headers, e.g. `MSG_HEADERS["content-type"]` (repeated headers are joined with `, `) |
| `MSG_BODY` | the value of the message |

The value is parsed with the input format given by `-i`:

- text (default): one record per line, split with `FS`
- `-i json`/`-i jsonl`: `$FI["name"]`, as for files
- `-i csv`/`-i tsv`: `$1`, `$2`...; with `-H` the first line of each value is its header

## Example: alert on error logs

Applications send JSON logs to the topic `logs`:

```json
{"level":"error","msg":"disk full","host":"web-1"}
```

```shell
zawk -i json '
  $FI["level"] == "error" {
      errors[$FI["host"]]++
      print f"[{strftime(\"%H:%M:%S\")}] {$FI[\"host\"]}: {$FI[\"msg\"]}"
  }
  END { for (host, n in errors) print host, n " errors" > "/dev/stderr" }
' 'kafka://localhost:9092/logs?group=alerts&offset=earliest'
```

## Bridge Kafka to NATS or MQTT

`publish()` sends to NATS and MQTT, so a topic can be forwarded with one rule:

```shell
zawk -i json '{ publish("mqtt://127.0.0.1:1883/devices/" $FI["device"], MSG_BODY) }' \
  'kafka://localhost:9092/telemetry?group=bridge'
```

## Tips

- With `?group=`, offsets are committed automatically every few seconds and when zawk exits. A message is
  committed once it is read, so a crash may skip the message being processed, or read recent ones again.
- The broker must advertise an address that zawk can reach: a broker in Docker needs
  `KAFKA_ADVERTISED_LISTENERS` set to the published host and port.
- A broker that cannot be reached within 10 seconds is a fatal error at start.
- A value that cannot be parsed (invalid JSON for `-i json`) is reported as a warning and in `ERRNO`, which is
  cleared when the next message starts. A message without a value has no records.
- A message key is not available to the program.
- TLS, SASL and several bootstrap servers are not supported, and `publish()` cannot send to Kafka.
