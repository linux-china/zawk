---
title: Text Parsers
sidebar_label: Parsers
---

# Text Parsers

Real-world text is full of small structured values: URLs, query strings, cookies, semantic versions,
Prometheus labels, command lines. zawk parses them into arrays, so you can index the result directly.

Use `var_dump(x)` (alias `pprint`) to look at the structure of any result on stderr:

```shell
zawk 'BEGIN { var_dump(semver("1.2.3-alpha.1+zstd.1.5.0")) }'
```

## URL

`url(text)` returns `schema`, `user`, `password` (percent-decoded), `host`, `port`, `path`, `query` and `fragment`.

```shell
zawk '{ u = url($1); print u["host"], u["path"], pairs(u["query"], "&")["id"] }' urls.txt
```

```awk
url("jdbc:mysql://localhost:3306/test")["port"]     # 3306
data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")["data"]   # also mime_type, encoding
```

## Pairs: query strings, cookies, tracestate

```awk
pairs("a=b,c=d")["c"]                              # d
pairs("id=1&name=Hello%20World", "&")["name"]      # Hello World (URL-decoded with "&")
pairs(cookies, ";", "=")["user_session"]
q = pairs("a=1&flag", "&"); print ("flag" in q)   # 1 (a key without value is kept as "")
```

## Records and messages

Prometheus/OpenMetrics-like text becomes an array:

```awk
r = record("http_requests_total{method='post',code=200}")
# r["method"] = "post", r["code"] = "200"
db = record("mysql{host=localhost user=root password=123456 database=test}")
m = message("login_event{method='post',code=200}('xxx@example.com')")   # name, headers and body
```

## Grok: logs and semi-structured text

`grok(s, pattern[, defs])` matches a [grok](https://www.elastic.co/guide/en/logstash/current/plugins-filters-grok.html)
expression and returns its named captures. `%{NAME:key}` uses the pattern `NAME` of the built-in library
(`IP`, `IPORHOST`, `HOSTNAME`, `WORD`, `INT`, `NUMBER`, `URIPATHPARAM`, `EMAILADDRESS`, `TIMESTAMP_ISO8601`,
`HTTPDATE`, `LOGLEVEL`, `QS`, `GREEDYDATA`, `COMMONAPACHELOG`, `COMBINEDAPACHELOG`, `SYSLOGLINE`, ...) and keeps
what it matched under `key`; `%{NAME}` without a key matches but is not kept. Plain regex can be mixed in.

```shell
zawk '{ m = grok($0, "%{COMMONAPACHELOG}"); print m["clientip"], m["verb"], m["request"], m["response"] }' access.log
```

```awk
m = grok("GET /index.html 200", "%{WORD:method} %{URIPATHPARAM:path} %{NUMBER:status:int}")
# m["method"] = "GET", m["path"] = "/index.html", m["status"] = "200"
```

- A `:type` suffix (`%{NUMBER:status:int}`) is accepted for Logstash compatibility and only names the key
  `status`: values are strings, which AWK converts to numbers when used as numbers.
- Text that does not match gives an empty array (`length(m) == 0`). An optional part that did not match is left
  out, so `("key" in m)` tells it from an empty match.
- The search is not anchored, but the parts of one expression match in order: `%{EMAILADDRESS:email} %{INT:n}`
  fails on text where the number comes first. To extract values whose order is unknown, call `grok` once per value.
- `defs` adds or overrides patterns, one `NAME regex` per line as in a Logstash patterns file (blank lines and
  `#` comments are skipped); load a file with `read_all`:

```awk
p = grok($0, "%{PHONE:phone}", "PHONE \\+?[0-9][0-9-]{6,15}")
m = grok($0, "%{ORDER_ID:oid} %{GREEDYDATA:msg}", read_all("patterns/app.grok"))
```

An unknown pattern (`%{NOPE}`), an invalid regex or a bad definition gives an empty array with a warning and
`ERRNO` set. Compiled expressions are cached, so calling `grok` on every record compiles each expression once.

To split every input line with a grok expression, use `--grok PATTERN` (with `--grok-defs FILE` for custom patterns)
instead: the captures become the columns, read as `$FI["key"]`, and lines that do not match are skipped:

```shell
zawk --grok '%{COMMONAPACHELOG}' '{ c[$FI["response"]]++ } END { for (s, n in c) print s, n }' access.log
```

See the [Grok tutorial](../tutorials/grok.md).

Use `grok` for logs and reusable patterns; for a one-off regex, `match(s, re, arr)` with named groups
`(?P<name>...)` or `rparse` is enough.

## Paths, versions and command lines

```awk
p = path("./demo.txt")       # exists, full_path, parent, file_name, file_stem, file_ext, content_type
path_join("logs", "2024", "app.log")   # "logs/2024/app.log"; an absolute part restarts the path
v = semver("1.2.3-alpha")    # major, minor, patch, pre, build
args = shlex("ls -l 'my file'")    # args[1] = ls, args[2] = -l, args[3] = my file
```

## Literal values

| Function | Input | Result |
|----------|-------|--------|
| `parse_array(s)` | `"['first','second','third']"` | array indexed from 1 |
| `tuple(s)` | `"(1,2,'first','second')"` | array indexed from 1 |
| `func(s)` | `"welcome('Jackie Chan',3)"` | `[0]` = `welcome`, `[1]` = `Jackie Chan`, `[2]` = `3` |
| `variant(s)` | `"week(5)"` | `name` = `week`, `value` = `5` |
| `flags(s)` | `"{vip,top20}"` | `vip` = 1, `top20` = 1 |
| `mkbool(s)` | `"yes"`, `"off"`, `"✓"` | `1`, `0`, `1` |

These also cover [Nushell](https://www.nushell.sh/) values: `duration("2min + 32sec")`,
`parse_array("[0 1 'two' 3]")`, `to_bytes("1.5GB")`, `record("{name:'Nushell', lang: 'Rust'}")`.

## Configuration files

```awk
conf = read_config("app.properties")    # also *.ini and *.json
print conf["db.url"]
```

`.env` in the current directory is always loaded as environment variables: read it with `ENVIRON["NAME"]` or
`getenv("NAME", "default")`.
