---
title: Calling HTTP APIs
sidebar_label: HTTP APIs
sidebar_position: 4
---

# Calling HTTP APIs

## Fetch data

`http_get` returns an array with `status`, `text` and the response headers in lower case:

```shell
$ zawk 'BEGIN { r = http_get("https://httpbin.org/get"); print r["status"], r["content-type"] }'
200 application/json
```

Combine with `json_value` (JSONPath) to extract fields:

```shell
zawk 'BEGIN {
    r = http_get("https://api.github.com/repos/linux-china/zawk")
    print json_value(r["text"], "$.stargazers_count"), "stars"
}'
```

## Enrich records

Call an API for every record. Here `ids.txt` holds one user id per line:

```shell
zawk '{
    r = http_get("https://jsonplaceholder.typicode.com/users/" $1)
    if (r["status"] != 200) { print $1, "error", r["status"] > "/dev/stderr"; next }
    print $1, json_value(r["text"], "$.name"), json_value(r["text"], "$.address.city")
}' ids.txt
```

## Send data

`http_post(url, body, headers)`. A body that looks like JSON gets `Content-Type: application/json`:

```shell
# CSV rows -> JSON -> webhook
zawk -i csv -H '{
    e["name"] = $FI["name"]; e["age"] = $FI["age"] + 0
    r = http_post("https://httpbin.org/post", to_json(e))
    print $FI["name"], r["status"]
}' users.csv
```

With authentication headers:

```awk
BEGIN {
    h["Authorization"] = "Bearer " getenv("API_TOKEN")
    h["Content-Type"] = "application/json"
}
{ http_post("https://api.example.com/events", f"{{\"line\": {NR}, \"text\": \"{escape(\"json\", $0)}\"}}", h) }
```

Put secrets such as `API_TOKEN` in `.env`: zawk loads it automatically.

## Errors

- A **network error** gives status `0` and sets `ERRNO`. It is a warning, not a crash.
- An **HTTP error** (404, 500) is a valid response: check `r["status"]`.

```awk
r = http_get(url)
if (r["status"] == 0)        print "network error: " ERRNO > "/dev/stderr"
else if (r["status"] >= 400) print "http error: " r["status"] > "/dev/stderr"
else                         print r["text"]
```

When you only need the body, `read_all(url)` fails on network errors **and** on a status ≥ 400, which makes
`try` a one-liner:

```awk
config = try read_all("https://example.com/config.json") else "{}"
```

Debug requests with `RUST_LOG=reqwest=debug`.

## Scrape HTML and XML

```shell
zawk 'BEGIN { r = http_get("https://example.com"); print html_value(r["text"], "h1") }'
zawk 'BEGIN { r = http_get("https://example.com/feed.xml"); t = xml_query(r["text"], "//item/title"); for (i = 1; i <= length(t); i++) print t[i] }'
```

## Remote input files and programs

Input files and even programs can be URLs:

```shell
zawk -i csv -H '{ print $FI["name"] }' https://example.com/data/users.csv
zawk -f https://example.com/scripts/report.awk data.txt
```

Running a program from a URL has the same risks as `curl ... | sh`: use URLs you trust, prefer `https://`, and
pin a version (a tag or commit) for reproducible runs.
