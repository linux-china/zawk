---
title: Network, Messaging & AI
sidebar_label: Network
---

# Network, Messaging & AI

## HTTP

```awk
r = http_get(url)                       # or http_get(url, headers)
r = http_post(url, body, headers)
print r["status"], r["text"], r["content-type"]
```

The response array holds `status` (`0` means a network error), `text` (the body) and the response headers in
lower case. A JSON body (starting with `{` or `[`) gets `Content-Type: application/json` automatically.

```shell
# Call an API per record and extract a value from the JSON response
zawk '{ r = http_get("https://api.example.com/users/" $1); print $1, r["status"], json_value(r["text"], "$.name") }' ids.txt

# With headers
zawk 'BEGIN { h["Authorization"] = "Bearer " ENVIRON["TOKEN"]; print http_get("https://api.example.com/me", h)["text"] }'
```

`read_all(url)` is the short form when you only need the body; it fails (and `try` catches it) on a status ≥ 400:

```awk
body = try read_all("https://example.com/data.json") else "{}"
```

See the [HTTP API tutorial](../tutorials/http-api.md).

## Email

```awk
send_mail("bot@example.com", "ops@example.com,dev@example.com", "Daily report", body)
send_mail(from, to, subject, body, "smtps://user:password@smtp.example.com:465")
```

The transport is the `smtp_url` argument, else the `SMTP_URL` environment variable, else
[MailerSend](https://www.mailersend.com/) (`MLSN_API_KEY`) or [Resend](https://resend.com/) (`RESEND_API_KEY`).

## S3

```awk
text = s3_get("bucket1", "reports/2026-10.csv")
s3_put("bucket1", "reports/summary.json", to_json(summary))
```

Settings come from `S3_ENDPOINT`, `S3_ACCESS_KEY_ID`, `S3_ACCESS_KEY_SECRET`, `S3_REGION` (or the `AWS_*` names),
also from `.env`. Input files can be read from S3 directly: see [Cloud data](../tutorials/cloud-data.md).

## Publish: NATS, Kafka, MQTT and desktop notifications

```awk
publish("nats://127.0.0.1:4222/orders.created", to_json(order))
publish("kafka://localhost:9092/orders", to_json(order))
publish("mqtt://127.0.0.1:1883/sensors/temp", $2)
publish("mqtts://BROKER_TOKEN@YOUR-BROKER.YOUR-NAMESPACE.cloudflarepubsub.com/topic", body)
publish("desktop://Job finished", "rows: " NR)
```

`publish` accepts `nats://`, `nats+tls://`, `kafka://`, `mqtt://`, `mqtts://` and `desktop://`. NATS subjects can also be
read as input; see the [NATS tutorial](../tutorials/nats.md). Kafka topics too; see the
[Kafka tutorial](../tutorials/kafka.md).

## AI decisions: `jev`

Typed LLM decisions per record with the [TypeSafe](https://docs.typesafe.ai/api) API (`TYPESAFE_API_KEY` is
required). The first argument is a text, or an array sent as a JSON object.

| Function | Returns |
|----------|---------|
| `jev(x, question)` | `1` when the probability of yes is above `jev_threshold` (default `0.5`) |
| `jev_prob(x, question)` | probability of yes, `0..1` |
| `jev_choice(x, question, options)` | the picked option |
| `jev_score(x, question, levels)` | probability-weighted position on ordered levels |

```shell
zawk 'jev($0, "the name is European")' names.txt
zawk '{ print jev_choice($0, "which team should handle this?", "billing,technical,security,sales"), $0 }' tickets.txt
```

More in the [AI integration tutorial](../tutorials/ai.md).

## Local IP

```awk
local_ip()      # 192.168.1.3
```
