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

- **Uninitialized values at join points** may print as `0`: `if (0) { x = 5 }; printf "[%s]", x` prints `[]` in
  AWK and `[0]` in zawk.
- **Missing elements of numeric arrays** print `0`, and referencing them creates them. Test with `(k in a)`,
  which never creates the element.
- **A string that looks like zero is false**: `if ("0")`, `if ("0.0")` and `if ("0x0")` are false (gawk treats
  every non-empty string constant as true). Use `if (s != "")`.
- **Hexadecimal numbers** are recognized in source and in input data: `0x10` is 16. A leading zero is decimal.
- In `for (k in a)`, `k` compares as a number when the keys are only ever numbers, and as a string otherwise.

## Regular expressions

zawk translates AWK regexes (POSIX ERE plus gawk's `\y`, `\<`, `\>`) to Rust's [regex](https://docs.rs/regex)
syntax, so Perl-style classes like `\d` and `(?i)` work too. One difference: alternation is
leftmost-**first**, not leftmost-longest. `match("xyz", /x|xy/)` sets `RLENGTH` to 1 in zawk and 2 in gawk.
Order alternatives longest first (`/xy|x/`) for the same result.

## Syntax

- `f"..."` is an f-string and `r"..."` a raw string, never a variable `f`/`r` concatenated with a string.
- `local`, `try` and `match ... {` are keywords.
- `..` and `..=` are range operators only in a `for (i in a..b)` header.
- Builtin functions need `(` right after the name: `length($1)`, not `length ($1)`.
- `-i` selects the input format, not gawk's `-i library`; use `@include` or `--include`.
- `getline line < dir "/" name` reads the concatenated path; parenthesize to be portable.

## Other differences

- With the default `FS`, `\r` separates fields, so CRLF line endings are not part of the last field.
- Output is batched aggressively for performance; call `fflush()` when you need output right away.
- zawk loads `.env` from the current directory as environment variables.
- Numbers are converted to strings with `OFMT`/`CONVFMT` as in AWK.

## Checking a program

`zawk lint` reports most of these cases with a dedicated code (W101, W103, W116, W117, W120, E202, E305...).
Run it when porting a gawk script:

```shell
zawk lint -f legacy.awk
```
