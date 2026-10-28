---
title: Syntax Overview
sidebar_label: Overview
---

# New AWK Syntax

zawk keeps the AWK language: patterns and actions, `BEGIN`/`END`, fields, associative arrays, `printf`,
`getline`, user-defined functions and gawk's `switch`. On top of it, zawk adds a small set of modern
constructs that make everyday scripts shorter and safer. Every addition is designed so that existing AWK
programs keep their meaning (the few exceptions are listed in [Differences from gawk](../compatibility.md)).

## At a glance

| Syntax | Example | Like |
|--------|---------|------|
| [f-string](./strings.md#f-strings) | `f"user {$1} at line {NR:05d}"` | Python f-strings |
| [Raw string](./strings.md#raw-strings) | `r"C:\Program Files"`, `$0 ~ r"\d+\.\d+"` | Rust raw strings |
| [Default value `??`](./operators.md#default-value-) | `"Hello " name ?? "Guest"` | C#/JS `??` |
| [Default assignment `??=`](./operators.md#default-assignment-) | `OFS ??= ","` | JS `??=` |
| [`try ... else`](./operators.md#try--else) | `body = try read_all(url) else ""` | Kotlin/Rust `?` |
| [`!in`](./operators.md#negated-membership-in) | `$1 !in seen { seen[$1]; print }` | Kotlin `!in` |
| [`match` expression](./match.md) | `match $2 { "error" \| "fatal" => 3, /^W/ => 2, _ => 0 }` | Rust `match` |
| [`switch` statement](./match.md#switch-statement) | `switch ($1) { case /^a/: ...; break }` | gawk |
| [`local` variables](./control-flow.md#local-variables) | `{ local total = 0; ... }` | JS `let` |
| [Key-value loop](./control-flow.md#key-value-iteration) | `for (k, v in count) print k, v` | Python `items()` |
| [Range loop](./control-flow.md#range-loops) | `for (i in 1..=10)` | Rust ranges |
| [Negative field index](./control-flow.md#negative-field-indexes) | `$(-1)`, `$(-2)` | Python `xs[-1]` |
| [`assert`](./control-flow.md#assert) | `assert(NF == 5, f"line {NR}: bad record")` | many languages |
| [Field list shorthand](./control-flow.md#field-list-shorthand) | `zawk '$1,$3' file` | `cut` |
| [Structured I/O](./structured-data.md) | `zawk -i csv -H '{ print $FI["name"] }'` | xsv, DuckDB |

## A taste

```awk
#!/usr/bin/env -S zawk -E
# @desc Summarize API latency per endpoint from a JSON Lines log

{
    local ms = $FI["latency_ms"] ?? 0
    level = match $FI["status"] {
        /^2/ => "ok",
        /^4/ => "client",
        /^5/ => "server",
        _ => "unknown"
    }
    lat[$FI["path"]] += ms
    hits[$FI["path"]]++
    errors[level]++
}

END {
    for (path, total in lat)
        print f"{path:<24} {hits[path]:>6} {total / hits[path]:>8.1f} ms"
    for (k, v in errors) print k, v > "/dev/stderr"
}
```

```shell
zawk -i jsonl -f latency.awk api.jsonl
```

## Keywords

`local`, `try` and `match ... {` are new keywords: they can no longer be used as variable or function names.
`f"..."` and `r"..."` are always string literals, so write `f "x"` (with a space) to concatenate a variable named
`f` with a string. `zawk lint` reports these cases (W103).
