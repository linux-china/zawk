---
title: zawk vs gawk
sidebar_label: zawk vs gawk
---

# zawk vs gawk

zawk and [gawk](https://www.gnu.org/software/gawk/) run the same AWK language, and most programs give the same
output in both. They are built for different jobs, though: gawk is the reference GNU AWK, available almost
everywhere, while zawk is a fast AWK with a standard library for structured data and services. This page helps
you pick one.

## At a glance

|                     | zawk                                                           | gawk                                               |
|---------------------|----------------------------------------------------------------|----------------------------------------------------|
| Language            | AWK + gawk extensions + modern syntax                          | AWK + gawk extensions                              |
| Implementation      | Rust, type inference, JIT with Cranelift                       | C, bytecode interpreter                            |
| Performance         | usually 2-8x faster on large files, parallel with `-p`         | single-threaded                                    |
| CSV / TSV           | RFC 4180 with `-i csv`, `-i tsv`, header names with `-H`       | `--csv` since 5.3                                  |
| Other input formats | JSON, JSON Lines, Parquet, Markdown tables, Excel              | text only                                          |
| Output formats      | CSV, TSV, JSON Lines, Markdown, table, Parquet with `-o`       | text only                                          |
| Input sources       | files, stdin, `http(s)://`, `s3://`, databases, NATS           | files, stdin, `/inet` sockets                      |
| Standard library    | ~190 functions: JSON, date/time, crypto, HTTP, SQL, KV, LLM... | strings, math, time, bit operations                |
| Extensions          | built in, no loading needed                                    | `@load` C extensions                               |
| Tooling             | `lint`, `fmt`, `inspect`, `functions`, `llm`                   | `--lint`, `--pretty-print`, `--profile`, `--debug` |
| Arbitrary precision | no                                                             | `-M` with MPFR                                     |
| Platforms           | 64-bit macOS, Linux, Windows                                   | almost every Unix-like system, often preinstalled  |

## Choose zawk when

- **The data is structured**: quoted CSV, JSON Lines, Parquet or Excel files, read with `-i` and addressed by
  column name instead of by position.

  ```shell
  zawk -i csv -H '{ total[$FI["city"]] += $FI["amount"] } END { for (c, t in total) print c, t }' orders.csv
  ```

- **The files are large**: type inference and machine code compilation make zawk faster than gawk on most
  workloads, and `-p r` / `-p f` spread the work over all cores. See the
  [benchmarks](https://github.com/linux-china/zawk/blob/master/info/performance.md).
- **The script talks to other systems**: HTTP APIs, S3, SQLite/MySQL/PostgreSQL, Redis/NATS KV, NATS/MQTT
  messages and LLMs, without shelling out to `curl` or a database client. See the
  [standard library](stdlib/overview.md).
- **You want a modern syntax**: f-strings, `??`, `try ... else`, `match`, `local` variables and
  `for (k, v in arr)`. See [New AWK Syntax](syntax/overview.md).
- **AI agents write or run the scripts**: `zawk inspect` shows the schema of a file, `zawk functions` lists the
  library, `zawk lint` reports problems as JSON and `zawk llm` builds a prompt. See [Tooling](tooling.md).

## Choose gawk when

- **The script must run anywhere** without installing anything: gawk, or another POSIX AWK, is already on most
  Linux systems and in most container images.
- **The script relies on gawk features zawk leaves out**: arrays of arrays (`a[i][j]`), `BEGINFILE`/`ENDFILE`,
  `FIELDWIDTHS`/`FPAT`, `RT`, co-processes (`|&`) and `/inet` networking, `@namespace`, indirect function calls
  (`@f()`), `@load` extensions, or i18n with `dcgettext()`.
- **You need exact numbers beyond doubles**: gawk's `-M` gives arbitrary precision arithmetic.
- **The script assigns to `$0`, fields or `NF`** to rebuild records, or edits `ARGV` to change the input files.
  These are read-only in zawk.
- **You need gawk's debugger or profiler** (`--debug`, `--profile`).
- **Exact POSIX semantics matter**, such as leftmost-longest regex alternation or treating `"0"` as a true
  string constant.

## Using both

Most AWK one-liners and scripts run unchanged in zawk, so you don't have to pick once for all:

- Keep portable scripts in plain AWK, and run them with zawk where it is installed for speed.
- Run `zawk lint -f script.awk` when moving a gawk script to zawk: it reports most behavior differences with a
  dedicated code.
- Pipe them into each other: zawk reads Parquet, JSON or a database table and writes CSV or TSV that any AWK
  can process, and the other way round.

The full list of behavior differences is in [Differences from gawk](compatibility.md).
