---
title: FAQ
---

# FAQ

## What is zawk?

A modern AWK implementation in Rust: `zawk = AWK + stdlib + Rust`. It is gawk mostly compatible, compiles
programs to machine code with Cranelift, reads structured formats (CSV, JSON, Parquet, Excel, databases, NATS, Kafka)
and ships a standard library of ~190 functions.

## How is it related to frawk?

zawk is based on Eli Rosenthal's [frawk](https://github.com/ezrosent/frawk), which brought the type inference,
the CSV/TSV support and the JIT backend. zawk continues it with a standard library, modern syntax, more
formats, gawk compatibility work and bug fixes. All frawk scripts run in zawk. Without frawk, there would be
no zawk.

## Why not just use gawk, Python or DuckDB?

- **gawk** has no CSV quoting before 5.3, no JSON, no Parquet, no HTTP or database functions, and is usually
  slower on large files.
- **Python** is great for programs, but heavy for one-liners: imports, CSV readers and loops for what AWK does
  in `'{ s[$1] += $2 } END { ... }'`.
- **DuckDB** is the best tool for SQL joins and window functions over many files. zawk complements it:
  line-oriented cleanup, logs, per-record side effects (HTTP, KV, messages). They pipe into each other through
  CSV.

Use `jq` for a single big JSON document and `sed -i` for in-place edits.

## Is it fast?

zawk infers types and JIT-compiles programs with Cranelift, and uses SIMD for CSV parsing and UTF-8
validation. It is usually 2-8x faster than gawk on large files, comparable to xsv on CSV, and can run scripts in
parallel with `-p r`. See the [benchmarks](https://github.com/linux-china/zawk/blob/master/info/performance.md).

## Which platforms are supported?

64-bit macOS, Linux and Windows. The default build picks SIMD fast paths at runtime, so it runs on any CPU of
the target architecture.

## Will my existing AWK scripts run?

Most will. See [Differences from gawk](./compatibility.md), and run `zawk lint -f script.awk` to find the
known incompatibilities.

## How do I process CSV with quoted fields?

```shell
zawk -i csv -H '{ print $FI["name"] }' data.csv
```

`-i csv` follows RFC 4180: commas, quotes and line breaks inside quoted fields are handled. Use `-o csv` to
write correctly quoted output.

## How do I query Apache Parquet?

```shell
zawk -i parquet '$FI["age"] > 30 { print $FI["name"], $FI["city"] }' family.parquet
```

`$1..$NF` are the top-level columns, `$0` is the row as a JSON object. See
[S3, Parquet & Excel](./tutorials/cloud-data.md).

## How do I read files from S3?

Use `s3://bucket/key` as the input file and set `S3_ENDPOINT`, `S3_ACCESS_KEY_ID`, `S3_ACCESS_KEY_SECRET` and
`S3_REGION` (or the `AWS_*` names), in the environment or in `.env`.

## How do I read a database table?

```shell
zawk '{ print $FI["name"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'
```

See [Working with databases](./tutorials/database.md).

## Why does a stdlib function return an empty value instead of failing?

Functions that talk to external services print a warning, set `ERRNO` and return an empty value, so a single
bad record or a flaky network call does not kill a long run. Use `try f() else fallback` to handle the failure
explicitly.

## How do I see what a function returns?

`var_dump(x)` (or `pprint(x)`) prints any value or array to stderr. `zawk functions NAME` shows the signature
and an example.

## How do I parse special values in text?

| Value | Function |
|-------|----------|
| bool | `mkbool("true")` |
| tuple | `tuple("('abc',123)")` |
| array | `parse_array("[1,2,3]")` |
| record | `record("{field1:1,field2:'two'}")` |
| variant | `variant("days(30)")` |
| flags | `flags("{read,write}")` |
| duration | `duration("2min + 32sec")` |
| file size | `to_bytes("1.5GB")` |

## How do I use zawk with Nushell?

Convert to CSV, then pipe to zawk:

```shell
ls | to csv | ^zawk -i csv '{ print $1 }'
```

## How do I use zawk with DuckDB?

```shell
duckdb -csv -c "from 'tests/demo.parquet'" | zawk -i csv 'NR > 1 { print $1 }'
./spring-log.awk spring.log | duckdb -c "select * from read_csv('/dev/stdin') where thread = 'main'"
```

## Can an LLM write zawk programs for me?

Yes: `zawk llm` builds a prompt from your data and question, `zawk lint` checks the answer, and a Claude Code
skill ships with the repository. See [AI integration](./tutorials/ai.md).

## Is it safe to run programs from a URL?

`zawk -f https://...` has the same risks as `curl ... | sh`: AWK programs can run commands, write files and use
the network. Only use URLs you trust, prefer `https://`, and pin a version for reproducible runs. As in gawk,
`system()` and pipes are not restricted: run untrusted programs inside an operating-system sandbox.

## Where are BLOBs from a database?

BLOB (BYTEA, binary) values are not supported yet and are read as empty strings.

## How can I contribute?

File issues and pull requests on [GitHub](https://github.com/linux-china/zawk). Adding a builtin function is
described in
[How to add a builtin function](https://github.com/linux-china/zawk/blob/master/info/How-to-add-builtin-function.md).
