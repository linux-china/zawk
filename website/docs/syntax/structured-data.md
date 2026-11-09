---
title: Structured Input & Output
sidebar_label: Structured I/O
---

# Structured Input & Output

Classic AWK splits records with regular expressions, which silently breaks on a CSV field such as
`"The Deluge: The Great War, America and the Remaking of the Global Order, 1916-1931"`. zawk has real parsers
for structured formats, and exposes columns by name through `FI`.

## Column names: `-H` and `FI`

With `-H`, the first line of each file is a header: it is not a record (`NR` is not incremented) and `FI` maps
column names to their index, so `$FI["age"]` is the field of the `age` column.

```shell
zawk -i csv -H '$FI["age"] > 40 { print $FI["name"], $FI["age"] }' users.csv
```

`-i jsonl`, `-i parquet`, `-i avro`, `-i markdown` and database inputs imply `-H`.

## Input formats

| Option | Format | Notes |
|--------|--------|-------|
| `-i csv` | CSV, RFC 4180 | quoted fields with `,`, `""` and line breaks |
| `-i tsv` | TSV | `\t`, `\n`, `\r`, `\\` escapes |
| `-i json` | JSON document | `data.json#$.items` selects records with a JSONPath |
| `-i jsonl` | JSON Lines (alias `ndjson`) | keys of the first record populate `FI`; nested values keep raw JSON text |
| `-i parquet` | Apache Parquet | `$0` is the row as a JSON object |
| `-i avro` | Apache Avro (Object Container File) | default for `.avro`; any codec; `$0` is the record as a JSON object |
| `-i markdown` (`md`) | first Markdown table of a file | |
| `-i xlsx` | Excel / ODS | default for `.xlsx`; `book.xlsx#Sheet2` selects a sheet |

```shell
# JSON Lines
zawk -i jsonl '$FI["level"] == "error" { print $FI["ts"], json_value($FI["ctx"], "$.user.id") }' app.jsonl

# Parquet to JSON Lines, or to CSV
zawk -i parquet '$FI["city"] == "Beijing" { print $0 }' people.parquet
zawk -i parquet -o csv '{ print $1, $2 }' people.parquet > people.csv

# Avro: `.avro` files are read as -i avro, also from stdin, HTTP(S) and S3 (streamed)
zawk '$FI["age"] > 40 { print $FI["name"] }' people.avro
cat people.avro | zawk -i avro -o csv '{ print $1, $2 }'

# A Markdown table in a README
zawk -i md '{ print $FI["name"] }' README.md

# An Excel sheet
zawk -H '$FI["age"] > 40 { print $FI["name"] }' people.xlsx
zawk -i xlsx '{ print $1, $2 }' 'book.xlsx#Sheet2'
```

## Input sources

Every input file can come from somewhere other than the local disk:

| Source | Example |
|--------|---------|
| stdin | `cat data.csv \| zawk -i csv '{ print $1 }'` |
| HTTP(S) | `zawk -i csv '{ print $1 }' https://example.com/data.csv` |
| S3 | `zawk -i csv '{ print $1 }' s3://bucket1/demo.csv` |
| SQLite | `zawk '{ print $FI["name"] }' 'sqlite://./app.db#user'` |
| MySQL | `zawk '{ print $FI["name"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'` |
| PostgreSQL | `zawk '{ print $1 }' 'postgresql://u:p@host:5432/db#select name, age from people'` |
| NATS | `zawk 'MSG_SUBJECT ~ /demo/ { print }' 'nats://localhost:4222/logs.>'` |
| Kafka | `zawk '{ print $1 }' 'kafka://localhost:9092/topic1'` |
| Compressed | `zawk '{ print $1 }' access.log.gz` (gz, zst, bz2, xz, lz4) |

See the [database](../tutorials/database.md), [NATS](../tutorials/nats.md), [Kafka](../tutorials/kafka.md)
and [cloud data](../tutorials/cloud-data.md) tutorials for details.

## Output formats

| Option | Format |
|--------|--------|
| `-o csv` | CSV, correctly quoted; records end with `\n` (use `-v 'ORS=\r\n'` for CRLF) |
| `-o tsv` | TSV with escapes, so any field survives a round trip |
| `-o markdown` (`md`) | Markdown table: the first printed record is the header row |
| `-o table` | aligned table with box-drawing borders, for terminals |
| `-o jsonl` | JSON Lines: the first printed record names the keys; a key may end with `:number` or `:boolean` |
| `-o parquet` | Apache Parquet, with `--out-file` |
| `-o avro` | Apache Avro (Object Container File, uncompressed), with `--out-file` |

```shell
# A Markdown table, ready for a PR, an issue or a chat reply
zawk -i jsonl -o md 'NR==1 { print "name", "age" } $FI["age"] > 40 { print $FI["name"], $FI["age"] }' users.jsonl
# | name | age |
# | --- | --- |
# | Jackie | 48 |

# CSV to Parquet: a column name may end with :TYPE
zawk -i csv -H -o parquet --out-file users.parquet \
  'NR==1 { print "id:int64", "name", "age:int32" } { print $1, $2, $3 }' users.csv
```

Supported Parquet column types: `BOOLEAN`, `INT8`, `INT16`, `INT32`, `INT64`, `FLOAT`, `DOUBLE`, `DATE`, `TIME`,
`TIMESTAMP`, `JSON`, `UUID` and `STRING` (the default).

`DATE` and `TIMESTAMP` columns (Parquet and Avro) take date and date-time text (`2024-01-15`,
`2024/01/15`, `2024-01-15 10:30:00`, RFC 3339 such as `2024-01-15T10:30:00+08:00`; naive values are UTC). A `DATE`
also takes 8 digits as `YYYYMMDD` (`20240115`), and any other bare number (`2024`, `1705276800`) is invalid. A
`TIMESTAMP` also takes a Unix timestamp: an integer is seconds, or milliseconds from 12 digits (absolute value
`>= 10^11`), microseconds from 15 digits and nanoseconds from 18 digits; a decimal such as `1705276800.5` is seconds.
So `20240115` in a `TIMESTAMP` column is 1970-08-23: write `2024-01-15`. Invalid values are written as null and
counted in a warning on standard error.

### Avro

`-i avro` maps the fields of the record schema to `$1..$NF` (and `FI`), and `$0` is the record as a JSON object:

| Avro type | awk text |
|---|---|
| `int`, `long`, `float`, `double` | the number |
| `boolean` | `1` / `0` |
| `string`, `enum` | the text, the symbol |
| union | the value of its branch |
| `date` | `2024-01-15` |
| `time-millis`, `time-micros` | `10:30:00.5` |
| `timestamp-*` | RFC 3339 in UTC: `2024-01-15T10:30:00Z` |
| `local-timestamp-*` | `2024-01-15 10:30:00` |
| `decimal` | exact decimal text: `12345.67` |
| `uuid` | `67e55044-10b1-426f-9247-bb680e5fe0c8` |
| `bytes`, `fixed` | hex |
| record, array, map | JSON text |
| null | empty (or the `--null` text) |

`-o avro` writes a record schema named `Row` whose fields are all nullable (`["null", T]`). The first printed record
is the header; a column name may end with `:TYPE` (case-insensitive):

| Suffix | Avro schema |
|---|---|
| `string` (default) | `string` |
| `int` / `int32` | `int` |
| `long` / `int64` | `long` |
| `float`, `double` | `float`, `double` |
| `boolean` | `boolean` (`true/t/yes/y/on` and nonzero numbers are true, `false/f/no/n/off` and `0` false) |
| `date` | `int` + `date` |
| `timestamp` | `long` + `timestamp-micros` (UTC) |
| `time` | `long` + `time-micros` |
| `uuid` | `string` + `uuid` |
| `decimal(P,S)` | `bytes` + `decimal` (rounded half away from zero to `S` digits; `decimal` alone is `decimal(18,3)`) |
| `bytes` | `bytes`, from hex text |
| `auto` | `long`, `double`, `boolean`, `date` or `string`, inferred from the first rows (`--infer-types` for all columns) |

The header is split at its last colon, and the suffix is a type only when it is one of these names: `url:http` is a column
named `url:http`. Avro names must match `[A-Za-z_][A-Za-z0-9_]*`, so other characters are replaced with `_` (`first name`
is `first_name`, `1st` is `_1st`, a repeated name gets `_2`), with a warning, and the original name is kept as the `doc`
of the field. Empty values and the `--null` text are null, except in `string` columns, which keep empty strings; values
that cannot be converted are null too, counted in a warning.

```shell
# CSV to Avro, and back
zawk -i csv -H -o avro --out-file users.avro \
  'NR==1 { print "id:long", "name", "age:int", "balance:decimal(12,2)" } { print $1, $2, $3, $4 }' users.csv
zawk '{ print $FI["name"], $FI["balance"] }' users.avro
```

```shell
# CSV to typed JSON Lines
$ zawk -i csv -H -o jsonl 'NR==1 { print "name", "age:number" } { print $FI["name"], $FI["age"] }' users.csv
{"name":"Jackie","age":48}
```

:::tip
With `-o csv`/`-o tsv`, `print a, b` quotes and escapes fields correctly. Never build CSV lines by concatenating
`","`. To escape a single value, use `escape_csv(s)`, `join_csv(i, j)` or `to_csv(arr)`.
:::

## Parallel execution

For scripts run with `-i csv`/`-i tsv`, scripts that only split by whitespace, or scripts that only use a
single-byte record and field separator, zawk can run the main loop in parallel:

```shell
zawk -i csv -p r -j 8 '{ sum += $3 } END { print sum }' big.csv
```

Scalars set in the main loop and read in `END` are aggregated over workers. `zawk lint -pr` reports code whose
meaning changes in parallel mode (W301, W302).
