---
title: New Operators
sidebar_label: ??, try, !in
---

# New Operators

## Default value: `??`

`l ?? r` is `r` when `l` is "empty", and `l` otherwise. `l` is evaluated once.

```shell
zawk '{ print "Hello " $2 ?? "Guest" }'      # "Hello " ($2 ?? "Guest")
zawk '{ n = cnt[$1] ?? -1 }'                 # an array element is not created by ??
zawk -v port=8080 'BEGIN { print host ?? "localhost", port ?? 80 }'
```

- **Empty** means uninitialized, `""`, or a number that is zero. A string that looks like zero (`"0"`, `"0.0"`,
  `"+0"`) is empty too.
- A field holding `0` is therefore replaced: for numeric columns where `0` is valid data, write
  `($3 == "" ? 0 : $3)`.
- An array element on the left is only read when it exists, as with `k in a`: `a[k] ?? "none"` does not create `a[k]`.
- `??` binds tighter than any other operator except `++`/`--`, and is left associative:
  `-u ?? 5` is `-(u ?? 5)`, `1 + u ?? 5` is `1 + (u ?? 5)`, `x ?? y ?? "z"` is `(x ?? y) ?? "z"`.
- The right operand is a term: parenthesize compound expressions, `u ?? (a + 1)`.

## Default assignment: `??=`

`l ??= r` is `l ?? (l = r)`: `r` is evaluated and assigned only when `l` is empty. The subscript of an array
element is evaluated once.

```awk
{
    OFS ??= ","
    total[$1] ??= 0
    first_seen[$1] ??= NR
}
```

## `try ... else`

Standard library functions that talk to the outside world (HTTP, databases, KV, S3, crypto keys, parsers) never
stop your program: on failure they print a warning to stderr, set `ERRNO` and return an empty value.
`try E else F` turns this into an expression:

```awk
body = try read_all(url) else ""
id   = try json_value(s, "$.id") else error_handler(ERRNO)
rows = try sqlite_query("app.db", "select name from users") else fail("query failed: " ERRNO)
try save(rec) else log_error(ERRNO)        # as a statement
```

How it works:

1. `ERRNO` is cleared and `E` is evaluated, **without printing warnings**.
2. If no stdlib function, `getline` or `close` failed inside `E`, the result is `E`, and `ERRNO` gets back its
   value from before the `try`.
3. Otherwise the result is `F`, which can read `ERRNO` (and `ERRNO` keeps the error).

Rules worth knowing:

- `else` is required, and `try` is a keyword.
- `E` may return an array (`http_get`, `sqlite_query`, `from_json`, ...): when it fails, a scalar `F` gives an
  empty array (`F` is still evaluated, e.g. to report the error), and an array `F` is the result. So both
  `try http_get(url) else ""` and `try http_get(url)["text"] else ""` work. A scalar `E` with an array `F` is an
  error.
- `F` extends to the end of the expression: `x = try f() else g() + 1` falls back to `g() + 1`. As an operand of
  another operator, parenthesize: `"id: " (try f() else "none")`.
- A failure handled by an inner `try` doesn't count for an outer one.
- `try` is not exception handling: fatal errors (division by zero, an array used as a scalar) still end the program.

:::tip
`http_get`/`http_post` only fail on a network error: an HTTP 404 or 500 is a valid response. Check
`r["status"]`, or use `try read_all(url) else ""`, which fails on a status ≥ 400.
:::

## Negated membership: `!in`

Like Kotlin's `!in` (Python's `not in`): `k !in arr` is the same as `!(k in arr)`.

```shell
zawk '$1 !in seen { seen[$1]; print }' data.txt       # first record of each key
zawk 'NR == FNR { block[$1]; next } $1 !in block' blocklist.txt users.txt
```

Multi-dimensional keys work with both forms: `(i, j) in arr`, `(i, j) !in arr`.

## Hexadecimal numbers

Hexadecimal literals are recognized in source and in input data wherever a string becomes a number:
`0x10` is 16, and `$1 == 16` is true for a field `0x10`. A leading zero is decimal (`010` is 10); use
`strtonum("017")` for octal.

## Bitwise functions

Following gawk, bitwise operations are functions: `and`, `or`, `xor`, `compl`, `lshift`, `rshift`, plus
`rshiftl` for a logical right shift. They work on 64-bit two's complement integers.
