---
slug: introducing-zawk
title: "zawk: AWK for the Age of CSV, Parquet, Databases and AI"
authors: [linux_china]
tags: [zawk, awk, release, ai]
---

AWK is almost fifty years old, and it is still one of the best tools ever made for "scan records, filter,
transform, aggregate, print". But the data we scan has changed: CSV with quoted commas, JSON Lines, Parquet
files on S3, tables in PostgreSQL, events on NATS. And the person writing the one-liner is increasingly an AI
agent.

**zawk** is a modern AWK built for that world: `zawk = AWK + stdlib + Rust`. Version 0.6.6 is out today, and
this post is a tour of what makes it different.

<!-- truncate -->

## It is still AWK

Everything you know keeps working:

```shell
zawk -F: '$3 >= 1000 { print $1 }' /etc/passwd
zawk '{ c[$9]++ } END { for (s in c) print s, c[s] }' access.log
zawk '!seen[$0]++' data.txt
```

zawk is gawk mostly compatible and the successor of Eli Rosenthal's [frawk](https://github.com/ezrosent/frawk).
It infers the types of your variables, compiles the program to machine code with Cranelift, and uses SIMD to
parse CSV and validate UTF-8. On large files it is usually **2-8x faster than gawk**, and simple scripts can run
in parallel with `-p r`.

## Real structured data

Classic AWK splits on a regex, which silently corrupts a CSV field like `"Doe, John"`. zawk has real parsers,
and columns have names:

```shell
zawk -i csv -H '$FI["age"] > 40 { print $FI["name"] }' users.csv
zawk -i jsonl '$FI["level"] == "error" { print $FI["ts"], $FI["msg"] }' app.jsonl
zawk -i parquet -o csv '{ print $1, $2 }' people.parquet > people.csv
zawk -H '{ print $FI["name"] }' 'book.xlsx#Sheet2'
```

Input can come from stdin, `https://`, `s3://`, compressed files, a **database table**, or a **NATS subject**:

```shell
zawk '$FI["age"] > 20 { print $FI["name"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'
zawk -i json 'MSG_SUBJECT ~ /orders/ { print $FI["id"] }' 'nats://localhost:4222/events.>'
```

And output can be CSV, TSV, JSON Lines, Parquet, an aligned terminal table, or a Markdown table ready to paste
into a pull request:

```shell
$ zawk -i csv -H -o md 'NR==1 { print "name", "age" } { print $FI["name"], $FI["age"] }' demo.csv
| name | age |
| --- | --- |
| Jackie | 48 |
| Love | 49 |
```

## A standard library

AWK has about twenty builtin functions. zawk has about **190**, grouped like a real standard library:

```shell
zawk '{ u = url($1); print u["host"], pairs(u["query"], "&")["id"] }' urls.txt
zawk '{ print uuid("v7"), digest("sha256", $0), encode("base64", $0) }' data.txt
zawk -i jsonl '{ print json_value($FI["payload"], "$.user.email") }' events.jsonl
zawk '{ lat[NR] = $NF } END { print _median(lat), _quantile(lat, 0.99) }' api.log
zawk '{ r = http_get("https://api.example.com/users/" $1); print json_value(r["text"], "$.name") }' ids.txt
zawk -i csv -H '{ p[1] = $1; p[2] = $2; sqlite_execute("app.db", "insert into users values(?, ?)", p) }' users.csv
```

Text, case conversion, masking, fuzzy matching, JSON/XML/HTML, dates, durations, crypto and JWT, IDs,
statistics, HTTP, email, S3, SQLite/MySQL/PostgreSQL, Redis/NATS KV, NATS/MQTT publishing... and functions
that fail never crash a long run: they warn, set `ERRNO`, and return an empty value.

## Modern syntax, without breaking AWK

zawk adds a handful of constructs that make scripts shorter and safer:

```awk
{
    local ms = $FI["latency_ms"] ?? 0                       # local variables, default values
    level = match $FI["status"] {                           # match expressions
        /^2/ => "ok", /^4/ => "client", /^5/ => "server", _ => "unknown"
    }
    body = try read_all($FI["url"]) else ""                 # explicit error handling
    if ($FI["user"] !in seen) seen[$FI["user"]]             # !in
    assert(NF >= 3, f"line {NR}: expect 3 fields")          # fail fast
}
END {
    for (k, v in errors) print f"{k:<10} {v:>8,}"           # key-value loops, f-strings with format specs
    for (i in 1..=3) print $(-i)                            # ranges, negative field indexes
}
```

Raw strings (`r"\d+\.\d+"`) end the double-backslash dance in dynamic regexes, and gawk's `switch` is there
too.

## Built for AI agents

The one-liner author of 2026 is often an LLM. zawk is designed to be easy for agents to use correctly:

- `zawk inspect data.csv` shows the schema, sample rows and a column summary.
- `zawk functions --json` lists every function with its signature and an example.
- `zawk lint` type checks a program and reports JSON diagnostics with codes, lines and carets.
- `zawk llm data.csv 'question'` builds the whole prompt: schema, sample, language notes and your question.

```shell
zawk -i csv -H "$(zawk llm -i csv -H demo.csv 'how many adults?' | llm -x)" demo.csv
zawk llm -i csv -H demo.csv 'what is the average age of adults?' | claude -p --dangerously-skip-permissions
```

A [Claude Code skill](https://github.com/linux-china/zawk/blob/master/skills/zawk/SKILL.md) ships with the
repository. And in the other direction, the `jev` functions let a zawk program ask typed questions of an LLM,
per record:

```shell
zawk '{ print jev_choice($0, "which team should handle this?", "billing,technical,security,sales"), $0 }' tickets.txt
```

## Try it

```shell
brew install linux-china/tap/zawk     # macOS
cargo binstall zawk                                   # anywhere with Rust tooling
```

Then head to the [Getting Started](/docs/intro) guide, the [syntax overview](/docs/syntax/overview), the
[standard library](/docs/stdlib/overview) and the [tutorials](/docs/tutorials/database). zawk has rough edges
and no one works on it full-time: [issues and pull requests](https://github.com/linux-china/zawk) are very
welcome.

Happy text gardening!
