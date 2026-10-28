---
title: match & switch
sidebar_label: match & switch
---

# `match` Expression and `switch` Statement

## `match` expression

`match subject { pattern => value, ... }` returns the value of the first arm whose pattern matches, like Rust's
`match`. Because it is an **expression**, it fits naturally into assignments, `print` and f-strings.

```shell
zawk '{ print match $2 { "info" => 1, "error" | "fatal" => 3, 3 => 3, /^W/ => 2, _ => 0 } }'
```

```awk
{
    level = match $3 {
        /^E/ => "error",
        "warn" | "W" => "warn",
        /(\d+)ms/ as m => m[1] > 1000 ? "slow" : "ok",
        _ => "info"
    }
    count[level]++
}
```

Rules:

- A pattern is a **string**, **number** or **regex** literal, or several joined with `|`. The first matching
  arm wins.
- Numbers compare numerically with a numeric-looking subject (a field, `getline` input...), as in gawk's
  `switch`: `3 => ...` matches `3.0`.
- `/re/ as m => value` binds the match to the array `m`: `m[0]` is the whole match, `m[1]` the first group.
  `m` is visible only in `value`.
- `_` matches anything and must be the last arm. Without `_`, the value is `""` when nothing matches.
- The subject is evaluated once. Arms are separated by commas or newlines.
- `match(s, re)` is still the builtin function; `match (x) {` starts a match expression.

### Examples

HTTP status classes:

```shell
zawk '{ c[match $9 { /^2/ => "2xx", /^3/ => "3xx", /^4/ => "4xx", /^5/ => "5xx", _ => "other" }]++ }
      END { for (k, v in c) print k, v }' access.log
```

Extract a number with a capture group:

```shell
$ echo "GET /api took 1532ms" | zawk '{ print match $0 { /(\d+)ms/ as m => m[1] / 1000 " s", _ => "-" } }'
1.532 s
```

## `switch` statement

zawk supports gawk's [switch statement](https://www.gnu.org/software/gawk/manual/html_node/Switch-Statement.html):

```awk
switch ($1) {
case 1:
case "one":
    print "one"
    break
case /^[0-9]+$/:
    print "a number"
    break
default:
    print "something else"
}
```

- The value is evaluated once and compared with each `case` label in order. A label is a numeric constant
  (`1`, `-2.5`, `0x10`), a string constant or a regex constant.
- Numeric and string labels are compared with `==`, regex labels with `~`.
- Control jumps to the first matching case, or to `default`, and then **falls through** until a `break`.
  `continue` inside a switch continues the enclosing loop.
- Duplicate labels (`1`, `1.0` and `"1"`) and a second `default` are syntax errors, as in gawk.

## Which one to use?

| | `match` | `switch` |
|---|---|---|
| Kind | expression (returns a value) | statement |
| Fall-through | never | until `break` |
| Regex groups | `/re/ as m` | no |
| Several patterns per arm | `"a" \| "b"` | stacked `case` labels |
| Portable to gawk | no | yes |

Use `match` to compute a value, and `switch` when each branch runs several statements or when the script must
also run with gawk.
