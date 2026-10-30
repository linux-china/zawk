---
title: Snippets
sidebar_label: Snippets
---

# zawk snippets


## Copy output to S3


```shell
# use s5cmd
zawk -i csv -o csv -H '{ print $1, $2 }' tests/demo.csv | zstd | s5cmd pipe s3://bucket1/demo2.csv.zst
# use aws s3
zawk -i csv -o csv '...' tests/demo.csv | zstd | aws s3 cp - s3://bucket1/2026-10-30.csv.zst
```

## DuckDB integration

```shell
# logging -> zawk structure -> DuckDB aggregate and write
zawk -o csv 'BEGIN{print "ts","level","latency"} /ERROR|WARN/ {print $1, $3, $NF}' app.log \
  | duckdb -c "COPY (SELECT * FROM read_csv('/dev/stdin')) TO 'out' (FORMAT parquet, PARTITION_BY (level))"

# DuckDB to Join/window，zawk for late process
duckdb -csv -c "SELECT o.*, c.email FROM 'orders.parquet' o JOIN 'customers.parquet' c USING (cid)" \
  | zawk -i csv -H '{ print $1 }'
```

## Shebang

```awk
#!/usr/bin/env -S zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta version 0.1.0
# @var nick current user nick
# @var email current user email
# @env S3_ENDPOINT s3 endpoint

BEGIN {
    print nick, email, ENVIRON["S3_ENDPOINT"]
}
```