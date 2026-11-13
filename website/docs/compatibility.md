---
title: Differences from gawk
sidebar_label: Differences from gawk
---

# Differences from gawk

zawk's language is borrowed almost wholesale from AWK: using it feels like using gawk, mawk or nawk, and most
programs produce the same output. This page lists what is intentionally different.

## Not supported

- `BEGINFILE` / `ENDFILE` blocks.
- `next` / `nextfile` inside functions (they work in the main rules).
- `FIELDWIDTHS` / `FPAT`: use `-i csv`, `substr()`, `match()` or `patsplit()`.
- `RT`.
- Arrays of arrays (`a[i][j]`): use `a[i, j]` with `SUBSEP`.
- Co-processes (`|&`).
- `$$1`, `$++i`: write `$($1)`.
- User-defined comparison functions in `PROCINFO["sorted_in"]` and `asorti` (gawk's predefined orders work).

## Read-only variables

- `ARGV`/`ARGC` are read-only: changing them doesn't change the input files.
- `$0`, fields and `NF`/`NR` are read-only: build a new string and print it instead (`print "x", $2`).
  `sub()`/`gsub()` on `$0` or a field are still allowed. `zawk lint` reports such assignments (E305).

## Types and values

zawk infers types before running the program. This is the main source of its speed, and it leaks into
behavior in a few places:

- **Uninitialized values** are both `0` and `""` as in AWK, including missing array elements and variables
  assigned on only some paths (`if (0) { x = 5 }; printf "[%s]", x` prints `[]`). A numeric variable that
  was never assigned still reads as `0` in a few places where gawk reads `""`:
  - passed to a user-defined function: `function g(s) { return s "|" }` with `if (0) u = 1; print g(u)`
    prints `0|`;
  - used as an array subscript: `if (0) u = 1; b[u] = 1` creates the key `0`;
  - as a `printf` / `sprintf` argument when the format is not a string literal, or uses positional
    arguments (`%1$s`).
- **A name is an array or a scalar for the whole program**, decided from every use of it, reachable or not:
  `if (0) { a = 2 } else { a[1] = 1 }` is rejected at compile time ("attempt to use array `a` in a scalar
  context"), while gawk only fails if a conflicting use actually runs. Use different names.
- **A string that looks like zero is false**, whether it is a constant, a string variable or a field:
  `if ("0")`, `if ("0.0")` and `if ("0x0")` are false, and `!"0"` / `!"0.0"` are `1` (gawk treats every
  non-empty string constant as true, so `!"0"` is `0` there). This is the difference most likely to bite when
  porting: a gawk script that tests for a non-empty string with `if (s)` takes the other branch when `s` is
  `"0"`. Write `if (s != "")` or `if (length(s))` instead.
- **Hexadecimal numbers** are recognized in source and in input data: `0x10` is 16, `"0x10" + 0` is 16 (gawk: 0),
  and for an input line `0x10 16`, `$1 == $2` is true (gawk compares them as strings). A leading zero is
  decimal: `010` in source is 10 (gawk: 8).
- **`printf` with too few arguments** is not fatal: zawk prints a warning and the missing arguments print as
  empty (or `0` for numeric conversions). `printf "%s|%s\n", "a"` prints `a|`. `zawk lint` reports it (W202).
- **A variable never assigned can become an array later**: `print length(x); x[1] = 1` works in zawk, while gawk
  stops with "attempt to use scalar as an array". Once a variable is assigned a scalar value, using it as an
  array is still an error, as in gawk.
- **Digit separators**: `1_000` is the number 1000 in source, not `1` joined with the variable `_000`. Write
  `1 _000` for the latter.
- In `for (k in a)`, `k` compares as a number when the keys are only ever numbers, and as a string otherwise.

## Regular expressions

zawk translates AWK regexes (POSIX ERE plus gawk's `\y`, `\<`, `\>`) to Rust's [regex](https://docs.rs/regex)
syntax, so Perl-style classes like `\d` and `(?i)` work too. As in gawk, a match is leftmost-longest:
`match("xyz", /x|xy/)` sets `RLENGTH` to 2.

One difference: `?` after a quantifier is not lazy as in Perl. zawk reads `a+?` as `(a+)?`, which still takes the
longest match, while gawk stops at the shortest. `gsub(/a+?/, "<&>")` on `aab` gives `<aa>b` in zawk and `<a><a>b`
in gawk. Spell out what a short match may contain instead: `/<[^>]+>/`, not `/<.+?>/`.

## Syntax

- `f"..."` is an f-string and `r"..."` a raw string, never a variable `f`/`r` concatenated with a string.
- `local`, `try` and `match ... {` are keywords.
- `..` and `..=` are range operators only in a `for (i in a..b)` header.
- Builtin functions need `(` right after the name: `length($1)`, not `length ($1)`.
- `-i` selects the input format, not gawk's `-i library`; use `@include` or `--include`.
- `getline line < dir "/" name` reads the concatenated path; parenthesize to be portable.

## Other differences

- **Carriage returns**: with the default `FS` (a single space), `\r` is a blank that separates fields, like
  space, tab and newline, so files with Windows (CRLF) line endings split cleanly. gawk splits on blanks and
  newlines only. For CRLF input:
  - the last field has no `\r`: for `a b\r\n`, `$2` is `b` (gawk: `b\r`), and `a\rb c` has 3 fields (gawk: 2);
  - a line holding only `\r` is blank: `NF` is 0, so `awk NF` drops it (gawk keeps it);
  - rebuilding the record (`$1 = $1`, assigning a field) drops the trailing `\r`;
  - `$0` itself keeps the `\r`, and the same rule applies to `getline`, `$0 = s` and `split(s, a)`.

  Set `FS = "[ \t\n]+"` to keep `\r` in the fields as gawk does (leading blanks then give an empty `$1`),
  or `sub(/\r$/, "")` at the start of the main rule to remove it explicitly.
- Output is batched aggressively for performance; call `fflush()` when you need output right away.
- zawk loads `.env` from the current directory as environment variables.
- Numbers are converted to strings with `OFMT`/`CONVFMT` as in AWK.

## Checking a program

`zawk lint` reports most of these cases with a dedicated code (W101, W103, W116, W117, W120, W202, E202, E305...).
Run it when porting a gawk script:

```shell
zawk lint -f legacy.awk
```
