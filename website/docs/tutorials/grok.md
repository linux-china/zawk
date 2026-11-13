---
title: Parsing Logs with Grok
sidebar_label: Grok
sidebar_position: 7
---

# Parsing Logs with Grok

Many logs have no reliable field separator: an access log mixes spaces, brackets and quoted strings, and an application
log ends with free text. [Grok](https://www.elastic.co/guide/en/logstash/current/plugins-filters-grok.html) describes
such a line with named patterns, as Logstash does:

```text
%{IP:client} %{WORD:method} %{URIPATHPARAM:path} %{NUMBER:status}
```

`%{NAME:key}` matches the pattern `NAME` of the built-in library (`IP`, `IPORHOST`, `WORD`, `INT`, `NUMBER`,
`URIPATHPARAM`, `HTTPDATE`, `TIMESTAMP_ISO8601`, `LOGLEVEL`, `QS`, `DATA`, `GREEDYDATA`, `COMMONAPACHELOG`,
`COMBINEDAPACHELOG`, `SYSLOGBASE`, ...) and keeps what it matched as `key`. `%{NAME}` matches without keeping
anything, and plain regex can be mixed in.

zawk can use grok in two ways:

- `--grok PATTERN` splits **every input line** with the pattern: the named captures are the columns, read as
  `$FI["key"]`.
- `grok(s, pattern[, defs])` matches **one string** and returns its captures as an array. See
  [Parsers](../stdlib/parsers.md#grok-logs-and-semi-structured-text).

## Split input with `--grok`

```text title="access.log"
127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326 "http://www.example.com/start.html" "Mozilla/4.08"
10.0.0.2 - - [10/Oct/2000:13:56:01 -0700] "POST /api/login HTTP/1.1" 401 64 "-" "curl/8.4.0"
10.0.0.2 - - [10/Oct/2000:13:56:09 -0700] "GET /index.html HTTP/1.1" 200 1024 "-" "curl/8.4.0"
# rotated at 2000-10-10
127.0.0.1 - frank [10/Oct/2000:13:57:12 -0700] "GET /index.html HTTP/1.1" 304 0 "-" "Mozilla/4.08"
```

`%{COMBINEDAPACHELOG}` matches the whole line. Count the status codes and sum the bytes per client:

```shell
$ zawk --grok '%{COMBINEDAPACHELOG}' '{ c[$FI["response"]]++ } END { for (s, n in c) print s, n }' access.log
200 2
304 1
401 1

$ zawk --grok '%{COMBINEDAPACHELOG}' '{ b[$FI["clientip"]] += $FI["bytes"] } END { for (ip, n in b) print ip, n }' access.log
10.0.0.2 1088
127.0.0.1 2326
```

`--grok` implies `-H`, as `-i jsonl` does. There is no header line in the input: zawk compiles the pattern
before reading and builds `FI` from its captures.

- **Columns**: `$1..$NF` are the captures in the order they appear in the pattern, including those of the patterns
  it refers to. `%{COMBINEDAPACHELOG}` gives `clientip`, `ident`, `auth`, `timestamp`, `verb`, `request`,
  `httpversion`, `rawrequest`, `response`, `bytes`, `referrer` and `agent`, so `$1` is the client and `$9` the status.
- **`$0`** is the whole line, unchanged.
- **Non-matching lines are skipped**, like the `# rotated` comment above: they never become records and do not
  count in `NR`.
- **Empty captures**: a capture that did not take part in the match is empty, for example an optional part:

```shell
$ printf 'GET /a 200\nGET /b\n' | zawk --grok '%{WORD:verb} %{URIPATH:path}( %{INT:status})?' '{ print NR, $FI["path"], "[" $FI["status"] "]" }'
1 /a [200]
2 /b []
```

- **Types**: a `:type` suffix (`%{NUMBER:bytes:int}`) is accepted for Logstash compatibility, but it only names the
  column `bytes`. Values are strings, and AWK converts them to numbers when used as numbers.
- **Repeated names**: a key used twice gets an index, as with `grok()`. The second `ip` below is `ip[1]`:

```shell
$ echo "10.0.0.1 -> 10.0.0.9" | zawk --grok '%{IP:ip} -> %{IP:ip}' '{ print $FI["ip"], $FI["ip[1]"] }'
10.0.0.1 10.0.0.9
```

An unknown pattern, an invalid regex or a pattern without any `%{NAME:key}` is reported before any input is read:

```shell
$ zawk --grok '%{IP} %{WORD}' '{ print }' access.log
zawk: invalid --grok pattern "%{IP} %{WORD}": no named captures: name the parts to extract, as in %{IP:client}
```

## Custom patterns with `--grok-defs`

For your own log format, write the patterns in a file, in the Logstash patterns file format: one `NAME regex` per
line, where the regex can use other patterns. Blank lines and `#` comments are skipped.

```text title="app.log"
2026-11-13 10:00:01,120 INFO  [order-service] order=A-1001 user=42 took=35ms Order created
2026-11-13 10:00:02,450 WARN  [order-service] order=A-1002 user=7 took=1250ms Payment slow
2026-11-13 10:00:03,001 ERROR [payment] order=A-1002 user=7 took=3001ms Gateway timeout
	at com.example.Gateway.call(Gateway.java:42)
2026-11-13 10:00:04,777 INFO  [order-service] order=A-1003 user=42 took=18ms Order created
```

```text title="app.grok"
# patterns for app.log
ORDER_ID [A-Z]-\d+
APPLOG %{TIMESTAMP_ISO8601:ts} %{LOGLEVEL:level}\s+\[%{DATA:service}\] order=%{ORDER_ID:order} user=%{INT:user} took=%{INT:took}ms %{GREEDYDATA:msg}
```

`--grok-defs` loads the file, and `--grok` then refers to its patterns by name. The stack trace line does not match
`APPLOG`, so it is skipped:

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok '$FI["level"] == "ERROR" { print $FI["ts"], $FI["order"], $FI["msg"] }' app.log
2026-11-13 10:00:03,001 A-1002 Gateway timeout

$ zawk --grok '%{APPLOG}' --grok-defs app.grok \
    '{ n[$FI["service"]]++; t[$FI["service"]] += $FI["took"] }
     END { for (s, c in n) printf "%-14s %d %.1fms\n", s, c, t[s] / c }' app.log
order-service  3 434.3ms
payment        1 3001.0ms
```

A definition with the name of a built-in pattern replaces it. To keep a pattern library in one place, put it next to
your scripts and reuse it with `--grok-defs`. The same text also works as the `defs` argument of `grok()`
(`grok($0, "%{APPLOG}", read_all("app.grok"))`).

## Convert logs to structured data

With an output format, a log becomes a table, JSON Lines, CSV or Parquet. The first printed record is the header
(see [Structured Data](../syntax/structured-data.md)):

```shell
$ zawk --grok '%{APPLOG}' --grok-defs app.grok -o table \
    'NR == 1 { print "order", "level", "took" } $FI["took"] > 1000 { print $FI["order"], $FI["level"], $FI["took"] }' app.log
┌────────┬───────┬──────┐
│ order  │ level │ took │
├────────┼───────┼──────┤
│ A-1002 │ WARN  │ 1250 │
│ A-1002 │ ERROR │ 3001 │
└────────┴───────┴──────┘

$ zawk --grok '%{COMBINEDAPACHELOG}' -o jsonl \
    'NR == 1 { print "client", "verb", "path", "status:number", "bytes:number" }
     { print $FI["clientip"], $FI["verb"], $FI["request"], $FI["response"], $FI["bytes"] }' access.log
{"client":"127.0.0.1","verb":"GET","path":"/apache_pb.gif","status":200,"bytes":2326}
{"client":"10.0.0.2","verb":"POST","path":"/api/login","status":401,"bytes":64}
...
```

For a Parquet file, use `-o parquet --out-file access.parquet`, then query it with DuckDB or any other Parquet reader.

## Find the lines that do not match

Skipped lines are counted as `rejected_records` by `--stats`, so you can check how much of a log a pattern covers:

```shell
$ zawk --stats --grok '%{COMBINEDAPACHELOG}' 'END { print NR }' access.log
4
{"input_records":4,"output_records":1,"rejected_records":1,...}
```

To see those lines, match each one with `grok()`. It returns an empty array when the line does not match:

```shell
$ zawk '!length(grok($0, "%{COMBINEDAPACHELOG}"))' access.log
# rotated at 2000-10-10
```

## Large logs

`--grok` reads lines, so it works with stdin (`tail -F app.log | zawk --grok ...`), compressed files, `s3://` and
`https://` inputs, and parallel runs:

```shell
zawk -p r -j 8 --grok '%{COMBINEDAPACHELOG}' '{ c[$FI["response"]]++ } END { for (s, n in c) print s, n }' access.log.zst
```

Matching a grok pattern is a regex search over the whole line, which is slower than splitting on a separator. When
the format allows it, anchor the pattern to the start of the line (`^%{IP:client} ...`) so that lines that cannot match
are rejected early, and prefer specific patterns (`INT`, `NOTSPACE`) to `DATA` or `GREEDYDATA` in the middle of
an expression.

## `--grok` or `grok()`?

| | `--grok PATTERN` | `grok(s, pattern[, defs])` |
|---|---|---|
| Matches | every input line | any string, such as `$0`, a field or a JSON value |
| Captures | columns: `$FI["key"]`, `$1..$NF` | an array: `m["key"]` |
| No match | the line is skipped | an empty array |
| Several formats in one log | one pattern for the whole input | a different pattern per line (`/sshd/ { m = grok(...) }`) |
| Custom patterns | `--grok-defs FILE` | `defs` text, e.g. `read_all("app.grok")` |

Use `--grok` when the whole log has one format. Use `grok()` for mixed logs, for a field of a CSV or JSON record, or
when non-matching lines must be handled rather than skipped.
