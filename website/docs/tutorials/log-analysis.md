---
title: Log Analysis
sidebar_label: Log analysis
sidebar_position: 6
---

# Log Analysis

Logs are where AWK shines. zawk adds the parsing, statistics and output formats to go from raw lines to a
report in one command.

## Access logs (NCSA Common Log Format)

```text
127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /apache_pb.gif HTTP/1.0" 200 2326
```

Status code distribution, with percentages:

```shell
zawk '{ c[$9]++ } END { for (s, n in c) printf "%s %d %.1f%%\n", s, n, 100 * n / NR }' access.log
```

Top 10 paths, as a Markdown table:

```shell
zawk -o md '{ c[$7]++ }
  END { print "path", "hits"; t = _top(c, 10); for (i = 1; i <= length(t); i++) print t[i], c[t[i]] }' access.log
```

Bytes per client, human readable:

```shell
zawk '{ b[$1] += $NF } END { for (ip, n in b) print ip, format_bytes(n) }' access.log
```

Convert to CSV for other tools:

```awk title="clf-log.awk"
#!/usr/bin/env -S zawk -E
BEGIN {
    OFS = ","
    print "host", "ident", "authuser", "date", "status", "bytes", "method", "protocol", "path"
}
{
    date = $4 $5
    method = trim($6, "\"")
    protocol = trim($(-3), "\"")
    path = join_fields(7, NF - 3, " ")
    print $1, $2, $3, trim(date, "[]"), $(-2), $(-1), method, protocol, path
}
```

## Latency percentiles

If the last field is the response time in milliseconds:

```shell
zawk '{ lat[NR] = $NF }
  END { print f"p50={_median(lat)} p90={_quantile(lat, 0.9)} p99={_quantile(lat, 0.99)} max={_max(lat)}" }' api.log
```

Per endpoint:

```shell
zawk '{ n = ++cnt[$7]; lat[$7, n] = $NF }
  END {
    for (p, n in cnt) {
      local xs[]
      for (i in 1..=n) xs[i] = lat[p, i]
      print f"{p:<30} {n:>6} p99={_quantile(xs, 0.99):.1f}"
    }
  }' api.log
```

## Application logs (Spring Boot)

```text
2024-04-08T13:32:46.674+08:00  INFO 16314 --- [spring-boot-demo] [           main] o.s.web.servlet.DispatcherServlet        : Initializing Servlet 'dispatcherServlet'
```

```shell
zawk '
  $1 !~ /^20\d\d-\d\d-\d\d/ { next }     # skip stack traces and blank lines
  {
    level = $2
    count[level]++
    if (level == "ERROR") print $1, join_fields(9, NF, " ") > "errors.txt"
  }
  END { for (l, n in count) print l, n }' spring.log
```

Pipe the CSV version into DuckDB for SQL:

```shell
./spring-log.awk spring.log | duckdb -c "select level, count(*) from read_csv('/dev/stdin') group by level"
```

## JSON logs

```shell
# errors per service per hour
zawk -i jsonl '$FI["level"] == "error" {
    hour = strftime("%Y-%m-%d %H:00", mktime($FI["ts"]), 1)
    c[hour, $FI["service"]]++
  }
  END { for (k, n in c) { split(k, p, SUBSEP); print p[1], p[2], n } }' app.jsonl | sort
```

Nested fields stay as JSON text: extract them with `json_value($FI["ctx"], "$.user.id")`.

## Classify lines with `match`

```shell
zawk '{
  kind = match $0 {
    /timeout|timed out/ => "timeout",
    /OutOfMemory|OOM/   => "memory",
    /refused|reset/     => "network",
    /(\d{3}) Internal/ as m => "http-" m[1],
    _ => "other"
  }
  c[kind]++
} END { for (k, n in c) print k, n }' errors.txt
```

## Follow a live log

zawk batches output for speed. When following a live log, act per record (publish, HTTP, KV) or call
`fflush()` after `print`:

```shell
tail -F app.log | zawk '/ERROR/ { publish("desktop://App error", $0) }'
tail -F app.log | zawk '/ERROR/ { print strftime("%H:%M:%S"), $0; fflush() }'
```

## Deduplicate

```shell
zawk '!seen[$0]++' data.txt          # exact, keeps first occurrence
zawk 'seen[$0]++ == 1' data.txt      # print each duplicated line once
zawk '!bf_icontains($0)' huge.txt    # constant memory, with a bloom filter
```
