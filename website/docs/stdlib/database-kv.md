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

MySQL and PostgreSQL connections are cached per URL. When a cached connection turns out to be closed,
`mysql_query` and `pg_query` reconnect and retry plain reads once (`SELECT`, `VALUES`, `TABLE`, `SHOW`, a `WITH`
that does not write; every statement of the call must be one); `*_execute` is never retried, and neither is a
`*_query` that writes (`INSERT ... RETURNING`, `CALL`, `INSERT ...; SELECT LAST_INSERT_ID()`), since the server may
have applied it before the connection was lost. The retry assumes a `SELECT` has no side effects: queries that call
`nextval`, `setval`, `pg_advisory_*`, `pg_notify`, `set_config` (PostgreSQL), `GET_LOCK`, `RELEASE_LOCK`, `SLEEP`,
`LAST_INSERT_ID`, `FOUND_ROWS` (MySQL) and similar built-ins are not retried, but a user-defined function that
writes cannot be detected, so run such a statement with `*_execute`.

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

If a value really has to be spliced into the SQL text (an identifier list, a generated `in (...)`), escape it
for the target dialect, and only as a fallback to parameters:

| Database | Escape |
|----------|--------|
| PostgreSQL, SQLite, DuckDB | `escape("sql", v)`: doubles `'` (standard SQL strings) |
| MySQL, MariaDB | `escape("mysql", v)`: also escapes `\`, NUL, `\n`, `\r`, `\x1a` |

`escape("sql", v)` is **not safe for MySQL/MariaDB**: there `\` escapes the next character, so a value like
`\' OR 1=1 -- ` breaks out of the string literal. Do not use `escape("mysql", v)` for PostgreSQL or SQLite either:
backslashes would be doubled in the stored value.

### Connections

Connections are cached per URL for the whole run. A PostgreSQL, MySQL or Redis connection that was closed is
reconnected once on the next call, so long-running input such as `tail -f` keeps working.
A MySQL or PostgreSQL connection closed **inside a transaction** (after `BEGIN`, before `COMMIT`/`ROLLBACK`) is
not reconnected silently: the server has rolled the transaction back, so that call fails, and so does every later
call to that URL from the same thread (including `COMMIT`), until the program runs `ROLLBACK` (which then succeeds)
or a new `BEGIN`. Reads are not retried inside a transaction either.

Each thread keeps its own MySQL and PostgreSQL connections, so a transaction can span several calls, also in a
parallel run (`-pr`/`-pf`) where every worker has its own session:

```awk
{
    pg_execute(url, "BEGIN")
    pg_execute(url, "insert into orders values(" $1 ", " $2 ")")
    pg_execute(url, "update stock set qty = qty - 1 where id = " $2)
    pg_execute(url, "COMMIT")
}
```

Without parameters, `sqlite_execute`, `mysql_execute` and `pg_execute` run several statements separated by `;`
in order and return the rows changed by all of them, e.g. `pg_execute(url, "BEGIN; insert ...; insert ...; COMMIT")`.
The first failing statement stops the run, and the statements before it stay applied (PostgreSQL applies the
statements of one call outside of an explicit transaction together or not at all), except when the call opened a
transaction: then it is rolled back, so `BEGIN; ...; COMMIT` applies all of its statements or none. A transaction
opened by an earlier call is left open for the program to `COMMIT` or `ROLLBACK`. With parameters the SQL is one
statement.

A transaction still open at exit (a `BEGIN` without `COMMIT`) is reported with a warning for all three databases,
since it is rolled back when the connection is closed. When more than one call of `*_execute` failed, a last
warning gives how many (the message of each distinct error is shown once): test `ERRNO` after a call to handle
each failure.

As for MySQL and PostgreSQL, each thread has its own connection to a SQLite database file, so a transaction
spanning several calls stays with the worker that opened it in a parallel run. Writers wait for each other's
transactions for up to 5 seconds before failing with `database is locked`: keep transactions short, and prefer
`BEGIN IMMEDIATE` when a transaction reads before it writes. An in-memory database (`:memory:`) is one connection
shared by all threads.

### Parallel runs and transactions

With `-pr`/`-pf`, `BEGIN` and `END` run in the main thread and the main rules in worker threads, each thread with its
own connections. A transaction opened in `BEGIN` (or ended in `END`) therefore does **not** include the writes of
the main rules: they autocommit one by one, a `ROLLBACK` in `END` does not undo them, and with SQLite they wait for
the lock of that open transaction and fail with `database is locked`. `zawk lint -p r` reports such a program
(`W306`), a run with `-p` prints the warning, and `--deny-warnings` refuses to run it.

```awk
# wrong with -p: the inserts of the workers are not in this transaction
BEGIN { pg_execute(url, "BEGIN") }
{ pg_execute(url, "insert into t values (" $1 ")") }
END { pg_execute(url, "COMMIT") }
```

For MySQL and PostgreSQL, open a transaction per worker in the main rules and commit it in `PREPARE`, which runs in
each worker at the end of its input (and once before `END` in a serial run):

```awk
{
    if (!tx) { tx = 1; pg_execute(url, "BEGIN") }
    p[1] = $1
    pg_execute(url, "insert into t values ($1)", p)
}
PREPARE { if (tx) pg_execute(url, "COMMIT") }
```

Each worker then commits its own share: the import as a whole is not all-or-nothing. When it must be, run the
program serially (`-p0`). SQLite has a single writer, so a transaction held by one worker blocks the others: run
SQLite imports serially, or make each transaction a single call (`sqlite_execute(db, "BEGIN; insert ...; COMMIT")`).

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
