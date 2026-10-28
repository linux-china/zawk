---
title: Standard Library Overview
sidebar_label: Overview
---

# Standard Library

AWK ships with about 20 builtin functions. zawk adds a standard library of **~190 functions**, inspired by the
standard libraries of DuckDB, ClickHouse, Go, Rust, Deno, PHP and Java's commons-lang, so that the common work
of data processing does not need a pipeline of other tools.

![zawk stdlib](https://raw.githubusercontent.com/linux-china/zawk/master/info/awk-stdlib.png)

## Categories

| Category | Functions |
|----------|-----------|
| [Text](./text.md) | `trim`, `truncate`, `pad`, `mask`, `starts_with`, `contains`, `camel_case`, `snake_case`, `title_case`, `similar`, `wrap`, `words`, `parse`, `rparse`, `format_bytes`, `figlet`, ... |
| [Parsers](./parsers.md) | `url`, `data_url`, `shlex`, `path`, `semver`, `pairs`, `record`, `message`, `func`, `tuple`, `variant`, `parse_array`, `flags` |
| [JSON / XML / HTML / CSV](./json-xml-html.md) | `from_json`, `to_json`, `json_flatten`, `json_value`, `json_query`, `json_set`, `xml_value`, `html_query`, `from_csv`, `to_csv` |
| [Array & Math](./array-math.md) | `seq`, `uniq`, `asort`, `asorti`, `_sum`, `_mean`, `_median`, `_quantile`, `_stddev`, `_top`, bloom filter, `abs`, `round`, `eval`, `fend`, `min`, `max` |
| [Date/Time](./datetime.md) | `systime`, `strftime`, `mktime`, `datetime`, `duration` |
| [Crypto & Encoding](./crypto-encoding.md) | `digest`, `hmac`, `encrypt`, `decrypt`, `jwt`, `dejwt`, `encode`, `decode`, `uuid`, `ulid`, `tsid`, `snowflake` |
| [Network](./network.md) | `http_get`, `http_post`, `send_mail`, `s3_get`, `s3_put`, `publish`, `local_ip`, `jev*` |
| [Database & KV](./database-kv.md) | `sqlite_*`, `mysql_*`, `pg_*`, `kv_get`, `kv_put`, `kv_delete`, `kv_clear` |
| [OS, I/O & Misc](./os-io.md) | `getenv`, `system2`, `read_all`, `write_all`, `read_config`, `var_dump`, `log_info`, `typeof`, `version`, `fake`, `hex2rgb` |

## Discover functions from the command line

Don't guess function names or argument orders: ask zawk. It is never out of date.

```shell
zawk functions                 # all functions, grouped by category: signature, summary, example
zawk functions json            # functions whose name contains "json"
zawk functions Crypto          # one category
zawk functions --json trim     # machine readable, one function per line
```

```json
{"name":"trim","category":"Text","signature":"trim(s: str[, chars: str]) -> str",
 "params":[{"name":"s","type":"str","optional":false},{"name":"chars","type":"str","optional":true}],
 "returns":"str","summary":"Remove whitespace, or any of chars, from both ends of s.","example":"trim(\"[hello]\", \"[]\")"}
```

There is also a printable [cheat sheet](https://cheatography.com/linux-china/cheat-sheets/zawk/).

## Conventions

**Arrays as results.** Functions documented as returning `map[...]` return an AWK array. Index the call directly
(`url($1)["host"]`) or assign it (`u = url($1)`). Use `var_dump(x)` to see the structure on stderr.

**Failures are warnings, not crashes.** Functions working with external services or user-supplied input
(network, KV, databases, crypto, parsers) print a warning to stderr and return an empty value (`""`, an empty
array, or `0` for an HTTP status). Each distinct warning is printed once. The failure also sets `ERRNO`:

```awk
ERRNO = ""
rows = sqlite_query("app.db", "select name from users")
if (ERRNO != "") { print "query failed: " ERRNO > "/dev/stderr"; exit 1 }

# or, in one expression and without the warning:
rows = try sqlite_query("app.db", "select name from users") else fail("query failed: " ERRNO)
```

**Your functions win.** A user-defined function with the same name as a stdlib function (such as `trim`, `max`
or `abs`) takes precedence over it in the whole program, so existing AWK programs that define these helpers keep
working. The POSIX/gawk builtins (`length`, `substr`, `split`, `gsub`, `sprintf`...) cannot be redefined.

**Parentheses are required.** Builtin functions must be followed directly by `(`: `length($1)`, not `length ($1)`.

**gawk functions.** `gensub`, `patsplit`, `strtonum`, `asort`, `asorti`, `systime`, `strftime`, `mktime`,
`typeof`, `isarray` and the bitwise functions behave as in gawk.
