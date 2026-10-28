---
title: Database & Key/Value
sidebar_label: Database & KV
---

# Database & Key/Value

## SQL functions

| Database | Query | Execute | URL |
|----------|-------|---------|-----|
| SQLite | `sqlite_query(db, sql[, params])` | `sqlite_execute(db, sql[, params])` | `app.db` (a path) |
| MySQL | `mysql_query(url, sql[, params])` | `mysql_execute(url, sql[, params])` | `mysql://root:123456@localhost:3306/test` |
| PostgreSQL | `pg_query(url, sql[, params])` | `pg_execute(url, sql[, params])` | `postgresql://postgres:postgres@localhost/db` |

`*_query` returns an array indexed from 1 where each row is a CSV-encoded string: split it with `from_csv(row)`.

```awk
BEGIN {
    rows = sqlite_query("app.db", "select id, name from users")
    for (i = 1; i <= length(rows); i++) {
        r = from_csv(rows[i])
        print r[1], r[2]
    }
}
```

### Parameters, never concatenation

All functions take an optional array of positional parameters, bound to a prepared statement in key order
(`p[1]`, `p[2]`...). Values containing `'` work, and input data cannot inject SQL.

```shell
zawk -i csv -H '
  BEGIN { sqlite_execute("app.db", "create table if not exists users(id int, name text)") }
  { p[1] = $1; p[2] = $2; sqlite_execute("app.db", "insert into users values(?, ?)", p) }' users.csv
```

Placeholders: `?` (or `?1`) for SQLite, `?` for MySQL, `$1`, `$2`... for PostgreSQL. PostgreSQL converts the
text values to the inferred parameter types (bool, integers, numeric, json, uuid, date/time...), with an empty
value as NULL for non-text types.

### Connections

Connections are cached per URL for the whole run. A PostgreSQL or Redis connection that was closed is
reconnected once on the next call, so long-running input such as `tail -f` keeps working.

## Tables and queries as input

A database URL can be an **input file**: a table (or a query, when the text after `#` contains spaces) is read
like `-i csv -H`.

```shell
zawk '{ print $FI["name"] }' 'sqlite://./app.db#user'
zawk '$FI["age"] > 20 { print $FI["name"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'
zawk '{ print $1 }' 'postgresql://postgres:123456@127.0.0.1:15432/demo_db#select name, age from people'
```

See the [database tutorial](../tutorials/database.md) for complete examples.

## Key/Value

```awk
kv_put(namespace, key, text)
kv_get(namespace, key)
kv_delete(namespace, key)
kv_clear(namespace)
```

The namespace selects the backend:

| Namespace | Backend |
|-----------|---------|
| `namespace1` | SQLite at `$HOME/.awk/sqlite.db` |
| `redis://localhost:6379/namespace` | a Redis hash named `namespace` (`redis://host:6379/0/namespace` for DB 0, `rediss://` for TLS) |
| `nats://localhost:4222/bucket_name` | a NATS JetStream KV bucket (create it with `nats kv add bucket_name`) |

```shell
# Only print new lines: remember the last processed line number between runs
zawk 'BEGIN { last = kv_get("redis://127.0.0.1:6379/jobs", "import.offset") + 0 }
      NR > last { print }
      END { kv_put("redis://127.0.0.1:6379/jobs", "import.offset", NR) }' events.log
```
