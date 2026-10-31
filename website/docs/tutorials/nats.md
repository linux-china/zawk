---
title: Streaming with NATS & MQTT
sidebar_label: NATS & MQTT
sidebar_position: 2
---

# Streaming with NATS & MQTT

AWK was born to process streams. With zawk, the stream can be a [NATS](https://nats.io/) subject, and the
output can be published back to NATS or MQTT. A ten-line AWK script becomes a message processor.

## Subscribe: a NATS subject as input

Pass a `nats://` URL as the only input file:

```shell
zawk 'MSG_SUBJECT ~ /demo/ { print }' 'nats://localhost:4222/logs.>'
```

- The subject may contain wildcards: `orders.*.created` (one token) or `orders.>` (one or more tokens).
- zawk reads messages until it receives **Ctrl+C** (SIGINT), then runs `END` and exits. The output of each
  message is written at once. A second Ctrl+C exits immediately (status 130) without running `END`.
- `?queue=name` subscribes as a member of a queue group, so several zawk workers can share the load:
  `nats://localhost:4222/orders.>?queue=workers`.

Each message is read as an **input file of its own**: `FNR` restarts at 1 and `FILENAME` is the subject.
These variables describe the message:

| Variable | Value |
|----------|-------|
| `MSG_SUBJECT` | the subject, e.g. `logs.app1` |
| `MSG_HEADERS` | the headers, e.g. `MSG_HEADERS["content-type"]` |
| `MSG_BODY` | the payload |

The payload is parsed with the input format given by `-i`:

- text (default): one record per line, split with `FS`
- `-i json`/`-i jsonl`: `$FI["name"]`, as for files
- `-i csv`/`-i tsv`: `$1`, `$2`...; with `-H` the first line of each payload is its header

## Example: alert on error logs

Applications publish JSON logs to `logs.<app>`:

```json
{"level":"error","msg":"disk full","host":"web-1"}
```

```shell title="Terminal 1: the processor"
zawk -i json '
  $FI["level"] == "error" {
      app = substr(MSG_SUBJECT, 6)
      errors[app]++
      print f"[{strftime(\"%H:%M:%S\")}] {app}: {$FI[\"msg\"]} on {$FI[\"host\"]}"
  }
  END { for (app, n in errors) print app, n " errors" > "/dev/stderr" }
' 'nats://127.0.0.1:4222/logs.>'
```

```shell title="Terminal 2: publish a test message"
zawk 'BEGIN { publish("nats://127.0.0.1:4222/logs.app1", "{\"level\":\"error\",\"msg\":\"disk full\",\"host\":\"web-1\"}") }'
# or: nats pub logs.app1 '{"level":"error","msg":"disk full","host":"web-1"}'
```

Terminal 1 prints:

```text
[14:21:07] app1: disk full on web-1
```

and after Ctrl+C, the `END` summary `app1 1 errors`.

## Publish: NATS, MQTT and desktop

`publish(url, body)` sends a message:

```awk
publish("nats://127.0.0.1:4222/orders.created", to_json(order))
publish("nats+tls://user:pass@nats.example.com:4222/audit", $0)
publish("mqtt://127.0.0.1:1883/sensors/temp", $2)
publish("mqtts://BROKER_TOKEN@YOUR-BROKER.YOUR-NAMESPACE.cloudflarepubsub.com/topic", body)
publish("desktop://Job finished", "rows: " NR)
```

### Turn a log file into events

```shell
# Ship nginx 5xx responses to NATS as JSON
tail -F /var/log/nginx/access.log | zawk '
  $9 ~ /^5/ {
      e["ip"] = $1; e["path"] = $7; e["status"] = $9; e["ts"] = systime()
      publish("nats://127.0.0.1:4222/http.errors", to_json(e))
  }'
```

### Bridge NATS to MQTT

```shell
zawk -i json '{ publish("mqtt://127.0.0.1:1883/devices/" $FI["device"], MSG_BODY) }' \
  'nats://127.0.0.1:4222/telemetry.>'
```

### Enrich and republish

```shell
zawk -i json '{
    u = url($FI["referrer"])
    out = json_set(MSG_BODY, "referrer_host", u["host"])
    publish("nats://127.0.0.1:4222/clicks.enriched", out)
}' 'nats://127.0.0.1:4222/clicks.raw?queue=enrichers'
```

Start several copies of this command: with `?queue=enrichers`, each message goes to one of them.

## NATS KV

The `kv_*` functions use a NATS JetStream KV bucket when the namespace is a `nats://` URL:

```shell
nats kv add config
zawk 'BEGIN { kv_put("nats://127.0.0.1:4222/config", "threshold", 100) }'
zawk 'BEGIN { t = kv_get("nats://127.0.0.1:4222/config", "threshold") } $3 > t' metrics.txt
```

## Tips

- If the program cannot keep up with the subject, NATS drops messages; zawk warns at the 1st, 10th, 100th...
  dropped message. Use a queue group to scale out.
- A payload that cannot be parsed (invalid JSON for `-i json`) is reported as a warning and in `ERRNO`, which is
  cleared when the next message starts.
- JetStream consumers and request/reply are not supported as input.
- Kafka topics can be read in the same way, see [Streaming with Kafka](./kafka.md).
