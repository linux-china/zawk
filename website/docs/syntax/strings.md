---
title: f-strings & Raw Strings
sidebar_label: f-strings & raw strings
---

# f-strings & Raw Strings

## f-strings

`f"..."` substitutes expressions into a string. It is clearer than `sprintf` or concatenation when you build
SQL, JSON, URLs, log lines or LLM prompts.

```shell
zawk '{ print f"user {$1} logged in at line {NR}" }' login.txt
zawk -v table=users '{ print f"SELECT * FROM {table} WHERE id = {$1};" }' ids.txt
zawk '{ print f"{{\"id\": {$1}}}" }' ids.txt            # {{ and }} are literal braces
zawk '{ print f"{NR+1}: {toupper($2)} {$3 * 1.1:.2f} {$1:>10}" }' data.txt
```

- A placeholder is **any expression**: `{name}`, `{$1}`, `{NR+1}`, `{toupper($2)}`, `{a[\"k\"]}`,
  `{x > 0 ? \"pos\" : \"neg\"}`. Write `\"` for a quote inside a placeholder (the f-string ends at a bare `"`).
- An invalid expression is a syntax error; `{}` stays as literal text.
- Escapes (`\t`, `\n`, `\"`) work as in normal strings.
- Values are not escaped: use `escape("sql", v)` or `escape("json", v)` first when the value comes from input
  (better: use [query parameters](../stdlib/database-kv.md) for SQL).

### Format specs

A format spec follows a `:` inside the placeholder, Python style:
`[[fill]align][sign][#][0][width][.precision][type]`.

| Placeholder | Value | Result |
|-------------|-------|--------|
| `{p:.2f}` | `3.14159` | `3.14` |
| `{n:5d}` | `42` | `   42` |
| `{n:<5}` | `42` | `42   ` |
| `{n:05}` | `42` | `00042` |
| `{s:*^6}` | `ab` | `**ab**` |
| `{n:x}` | `42` | `2a` |
| `{r:.1%}` | `0.256` | `25.6%` |
| `{s:.2}` | `hello` | `he` |
| `{n:,}` | `1234567` | `1,234,567` |
| `{n:_d}` | `1234567` | `1_234_567` |
| `{p:,.2f}` | `1234.5` | `1,234.50` |

Without a type the value is formatted as a string; values are right-aligned by default.
`printf`/`sprintf` also accept the `'` and `_` flags for grouping: `printf "%'d\n", 1234567` prints `1,234,567`.

:::caution
A misspelled name such as `{nmae}` silently becomes an empty string, like any uninitialized AWK variable.
`zawk lint` reports variables that are read but never set (W201).
:::

## Raw strings

Raw string literals work like Rust's: backslashes are **not** escapes. They are handy for Windows paths and
for dynamic regular expressions, where you would otherwise write `"\\d+"`.

```awk
path = r"C:\Program Files\Rust"
if ($0 ~ r"\d+\.\d+") print "version-like"
split($0, parts, r"\s*,\s*")
```

To include a `"`, put the same number of `#`s around the quotes:

```awk
print r#"say "hi""#          # say "hi"
print r##"a"#b"##            # a"#b
```

A raw string can span lines, which is convenient for embedded SQL or templates:

```awk
BEGIN {
    sql = r"
        select name, age
        from people
        where age > 18
    "
}
```

:::note
`r"..."` (no space) is always a raw string, never the variable `r` concatenated with a string. With `r = 5`,
gawk prints `5x` for `print r"x"` while zawk prints `x`. Write `r "x"` or rename the variable.
:::

## Unicode-aware strings

Text is UTF-8: `length`, `substr`, `index` and `match` count **characters**, and `strlen` counts bytes.

```shell
$ zawk 'BEGIN { print length("你好Hello"), substr("你好Hello", 1, 2), strlen("你好") }'
7 你好 6
```

Input that is not valid UTF-8 (GBK, Latin-1, binary) is kept byte for byte; use `--utf8` to validate input.
