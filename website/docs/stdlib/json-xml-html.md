---
title: JSON, XML, HTML & CSV
sidebar_label: JSON / XML / HTML / CSV
---

# JSON, XML, HTML & CSV

## JSON

| Function | Description |
|----------|-------------|
| `from_json(s)` | Parse an object (keys) or array (keys `1..n`) into an array. Strings are decoded, `true`/`false` become `1`/`0`, `null` becomes `""`, nested values stay as JSON text |
| `json_flatten(s)` | Flatten nested JSON into an array keyed by paths: `data.account.name`, `metadata[1].key` |
| `to_json(arr)` | Arrays with keys `1..n` become JSON arrays, others objects with sorted keys |
| `json_value(s, path)` | One value selected by an [RFC 9535 JSONPath](https://www.rfc-editor.org/rfc/rfc9535.html) |
| `json_query(s, path)` | All values selected by a JSONPath, as an array |
| `json_has(s, key)` / `json_get(s, key)` | Test / read a top-level key |
| `json_set(s, key, value)` / `json_del(s, key)` | Return new JSON text with the key set / removed |

```shell
# Pick nested values from JSON Lines
zawk -i jsonl '{ print $FI["id"], json_value($FI["payload"], "$.user.email") }' events.jsonl

# Flatten nested JSON
echo '{"data":{"account":{"name":"Jackie"}},"metadata":[{"key":"x"}]}' \
  | zawk '{ r = json_flatten($0); print r["data.account.name"], r["metadata[1].key"] }'
# Jackie x

# Redact a field and add one
zawk '{ print json_set(json_del($0, "password"), "processed_at", systime()) }' users.jsonl

# Build JSON from fields
zawk -F, '{ o["id"] = $1; o["name"] = $2; print to_json(o) }' users.txt
```

`json_set` keeps a value that is valid JSON text (`18`, `true`, `[1,2]`) as JSON, and stores any other text as a
JSON string.

## XML and HTML

```awk
xml_value(xml, "/book/title")          # inner text of the first node, XPath
xml_query(xml, "//book/author")        # array of all matches
html_value(html, "title")              # inner text of the first match, CSS selector
html_query(html, "li.item")            # array of inner texts
```

HTML selectors are simple selectors: a tag (`h1`), a class (`.item`, `li.item`), an id (`#main`) or an
attribute (`a[href]`). Combinators such as `ul > li` or `div p` are not supported.

Scrape a page in one line:

```shell
zawk 'BEGIN { r = http_get("https://example.com"); print html_value(r["text"], "h1") }'
```

## CSV

```awk
arr = from_csv("1,\"Doe, John\",42")   # arr[2] = Doe, John
print to_csv(arr)                       # 1,"Doe, John",42
print join_csv(1, 3)                    # fields 1..3, escaped
print escape_csv($2)
```

For whole files, prefer `-i csv` and `-o csv` (see [Structured I/O](../syntax/structured-data.md)).
