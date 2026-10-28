---
title: S3, Parquet & Excel
sidebar_label: S3, Parquet & Excel
sidebar_position: 5
---

# S3, Parquet & Excel

## Read from S3

Use `s3://bucket/key` as an input file, in any input format, or with `getline`:

```shell
zawk -i csv '{ print $1 }' s3://bucket1/demo.csv
zawk -i parquet '{ print $FI["name"] }' s3://bucket1/demo.parquet
zawk 'BEGIN { while ((getline line < "s3://bucket1/demo.csv") > 0) print line }'
```

Settings are read from environment variables, also from `.env` in the current directory:

```ini title=".env"
S3_ENDPOINT=http://127.0.0.1:9000
S3_ACCESS_KEY_ID=minioadmin
S3_ACCESS_KEY_SECRET=minioadmin
S3_REGION=us-east-1
```

The `OPENDAL_S3_*` and `AWS_*` names (`AWS_ENDPOINT_URL`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`AWS_REGION`) work too.

Objects are streamed: 8 MB parts are downloaded in parallel (4 at a time) while the program processes the
data, so memory stays bounded for large objects. A missing bucket or object, or denied access, is reported
before any input is processed.

Write results back with `s3_put`:

```shell
zawk -i csv -H '{ s[$FI["dept"]] += $FI["salary"] } END { s3_put("reports", "salary-by-dept.json", to_json(s)) }' \
  s3://hr/employees.csv
```

## Apache Parquet

```shell
zawk -i parquet '$FI["age"] > 30 { print $FI["name"], $FI["city"] }' family.parquet
zawk -i parquet -o csv '{ print $1, $2 }' family.parquet > family.csv
cat family.parquet | zawk -i parquet '{ print $0 }'     # each row as a JSON object: JSON Lines
```

Values are converted as with `-i jsonl`:

- numbers and strings as is, decimals exactly, booleans as `1`/`0`, null as `""`
- `DATE`, `TIME`, `TIMESTAMP` as text: `2024-01-15`, `13:45:30.5`, `2024-01-15 10:30:00` (UTC)
- `UUID` as text, other binary values as hex
- `LIST`, `MAP`, `STRUCT` and `JSON` columns as JSON text: process them with `json_value`

Compressions: SNAPPY, ZSTD, LZ4 and uncompressed. With several files, columns are matched by name with those of
the first file.

### Write Parquet

The first printed record is the header; a column name may end with `:TYPE`:

```shell
zawk -i csv -H -o parquet --out-file users.parquet \
  'NR == 1 { print "id:int64", "name", "age:int32", "created:timestamp" }
   { print $FI["id"], $FI["name"], $FI["age"], $FI["created_at"] }' users.csv
```

### Convert JSON Lines to Parquet

```shell
zawk -i jsonl -o parquet --out-file events.parquet \
  'NR == 1 { print "ts:timestamp", "level", "msg" } { print $FI["ts"], $FI["level"], $FI["msg"] }' events.jsonl
```

## Excel

`.xlsx` files are read as CSV (from column A); `-H` makes the first row the header:

```shell
zawk -H '$FI["age"] > 40 { print $FI["name"] }' people.xlsx
zawk -i xlsx '{ print $1, $2 }' 'book.xlsx#Sheet2'     # select a sheet (name matched ignoring case)
zawk inspect 'book.xlsx#Sheet2'                         # schema, sample rows, summary
```

Dates are `2024-01-05` or `2024-01-05 13:45:10`, times `08:30:00`, booleans `1`/`0`, empty cells `""`.
`.xls`, `.xlsm`, `.xlsb` and `.ods` are read too.

```shell
# Excel -> Markdown table for a PR description
zawk -H -o md 'NR == 1 { print "name", "age" } { print $FI["name"], $FI["age"] }' people.xlsx
```

## Compressed files

gzip, zstd, bzip2, xz and lz4 input is detected and decompressed automatically, locally, over HTTP and on S3:

```shell
zawk '$9 == 500' access.log.gz
zawk -i jsonl '{ print $FI["id"] }' s3://logs/2026/10/28/events.jsonl.zst
```
