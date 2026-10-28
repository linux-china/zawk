---
title: Working with Databases
sidebar_label: Databases
sidebar_position: 1
---

# Working with Databases

zawk talks to SQLite, MySQL and PostgreSQL in two ways:

1. **As input**: a table or query is read like a CSV file with a header.
2. **As functions**: `*_query` and `*_execute` run SQL from any rule, with bound parameters.

The examples use this table, in MySQL and PostgreSQL:

```sql
create table people (id int, name varchar(255), age int);

insert into people(id, name, age) values (1, 'Jackie', 48);
insert into people(id, name, age) values (2, 'Love', 48);
insert into people(id, name, age) values (3, 'Leo', 24);
insert into people(id, name, age) values (4, 'Lucas', 10);
insert into people(id, name, age) values (5, 'Laura', 8);
```

## 1. Read a table as input

Append `#table` to the database URL:

```shell
$ zawk '$FI["age"] > 20 { print $FI["name"], $FI["age"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'
Jackie 48
Love 48
Leo 24
```

When the text after `#` contains spaces, it is a SQL query:

```shell
$ zawk -o md 'NR == 1 { print "name", "age" } { print $FI["name"], $FI["age"] }' \
    'postgres://postgres:123456@127.0.0.1:15432/demo_db#select name, age from people where age > 20'
| name | age |
| --- | --- |
| Jackie | 48 |
| Love | 48 |
| Leo | 24 |
```

```shell
zawk '{ print $1, $2 }' 'sqlite://./app.db#select nick, age from user where age > 30'
```

- URLs: `sqlite://PATH`, `mysql://...`, `postgresql://...` (or `postgres://`).
- Rows are streamed; `FI` maps the column names, so no `-i` or `-H` is needed. NULL and BLOB values are `""`.
- Passwords are masked in messages.
- `zawk inspect URL` shows the schema with database column types, sample rows and a summary.

### Export a table

Because a table is just input, every output format works:

```shell
# MySQL table -> CSV
zawk -o csv 'NR == 1 { print "id", "name", "age" } { print $1, $2, $3 }' \
  'mysql://root:123456@127.0.0.1:3306/test#people' > people.csv

# PostgreSQL query -> Parquet
zawk -o parquet --out-file people.parquet \
  'NR == 1 { print "id:int32", "name", "age:int32" } { print $1, $2, $3 }' \
  'postgres://postgres:123456@127.0.0.1:15432/demo_db#people'
```

### Join a table with a file

AWK's classic two-file idiom works with a database as the first "file":

```shell
# orders.csv: person_id,amount
$ zawk -i csv -H 'NR == FNR { name[$1] = $2; next }
                  { total[name[$1]] += $2 }
                  END { for (n, t in total) print n, t }' \
    'mysql://root:123456@127.0.0.1:3306/test#people' orders.csv
Jackie 15
Leo 7
```

With several inputs, pass `-H` so that the first line of each CSV file is treated as its header, like the
column names of the table.

## 2. Query from a program

`mysql_query`, `pg_query` and `sqlite_query` return an array of rows, each row CSV-encoded:

```awk
BEGIN {
    db = "mysql://root:123456@127.0.0.1:3306/test"
    p[1] = 20
    rows = mysql_query(db, "select id, name from people where age > ?", p)
    for (i = 1; i <= length(rows); i++) {
        r = from_csv(rows[i])
        print r[1], r[2]
    }
}
```

Lookups per record are easy, and the connection is cached for the whole run:

```shell
# ids.txt has one person id per line
zawk -v db="postgres://postgres:123456@127.0.0.1:15432/demo_db" '
  { p[1] = $1; rows = pg_query(db, "select name from people where id = $1", p); print $1, from_csv(rows[1])[1] }' ids.txt
```

## 3. Write: load a CSV file into a database

```shell
zawk -i csv -H '
  BEGIN {
    db = "app.db"
    sqlite_execute(db, "create table if not exists users(id int primary key, name text, email text)")
  }
  {
    p[1] = $FI["id"]; p[2] = $FI["name"]; p[3] = $FI["email"]
    sqlite_execute(db, "insert or replace into users values(?, ?, ?)", p)
    n++
  }
  END { print n " rows loaded" > "/dev/stderr" }' users.csv
```

:::danger Don't build SQL with concatenation
`sqlite_execute(db, "insert into t values('" $1 "')")` breaks on `O'Brien` and lets the input inject SQL.
Bind values with a parameter array instead. Placeholders are `?` for SQLite and MySQL, `$1`, `$2`... for
PostgreSQL.
:::

## 4. Handle errors

Database functions never crash the program: they print a warning and set `ERRNO`. Use `try` to decide what
happens:

```awk
rows = try sqlite_query("app.db", "select * from users") else NONE
if (ERRNO != "") { print "query failed: " ERRNO > "/dev/stderr"; exit 1 }
```

## 5. Pipe to DuckDB

zawk and DuckDB complement each other: zawk cleans and reshapes line-oriented data, DuckDB does joins and
window functions.

```shell
# DuckDB -> zawk
duckdb -csv -c "from 'tests/demo.parquet'" | zawk -i csv 'NR > 1 { print $1 }'

# zawk -> DuckDB
./spring-log.awk spring.log | duckdb -c "select * from read_csv('/dev/stdin') where thread = 'main'"
```
