---
title: Tooling
---

# Tooling

zawk is more than an interpreter. Its subcommands help people and AI agents look at data, discover functions,
and check, format and document programs.

| Command | Purpose |
|---------|---------|
| `zawk inspect FILE` | schema, sample rows and column summary of a data file |
| `zawk functions [FILTER]` | list builtin functions with signatures and examples |
| `zawk lint` | parse and type check a program, JSON diagnostics |
| `zawk fmt` | format AWK programs |
| `zawk llm FILE... QUESTION` | build an LLM prompt to write a program |
| `zawk init NAME` | create a script from a template |

## `zawk inspect`

Look at a CSV, TSV, JSON Lines, Parquet, Excel file or database table before writing a program for it:

```shell
$ zawk inspect demo.csv
File:    demo.csv
Format:  csv (read with: zawk -i csv -H '...' demo.csv)
Rows:    5
Columns: 3

Schema:
┌───────┬──────┬────────┐
│ field │ name │ type   │
├───────┼──────┼────────┤
│ $1    │ id   │ INT64  │
│ $2    │ name │ STRING │
│ $3    │ age  │ INT64  │
└───────┴──────┴────────┘

Sample (first 5 rows):
...
```

The summary is like DuckDB's `SUMMARIZE`: min, max, approx_unique, avg, std, q25, q50, q75, count and
null_percentage. Options: `-i FORMAT`, `-o table|markdown`, `-n ROWS` (sample size), `--max-rows N`.
Compressed files, URLs and S3 objects are supported. A CSV header of `name:TYPE` declares the column type.

## `zawk functions`

```shell
zawk functions                 # all functions, grouped by category
zawk functions json            # names containing "json"
zawk functions Crypto          # one category
zawk functions --json trim     # one JSON object per function
```

## `zawk lint`

`zawk lint` parses and type checks a program without reading any input, and prints a JSON report. The exit
status is 0 when the program is valid (warnings allowed), 1 otherwise.

```shell
zawk lint '{ print $1 }'               # program text
zawk lint -f demo.awk                  # program files
echo 'BEGIN { print 1 }' | zawk lint   # from stdin
zawk lint -v n=10 -f top.awk           # n is set on the command line
zawk lint -H -f report.awk             # the run uses -H: $FI[...] is filled
zawk lint -pr -f sum.awk               # also report code that changes meaning in parallel
zawk --lint -f report.awk data.csv     # lint, then run if there is no error
```

### Diagnostics

| Code | Meaning |
|------|---------|
| E101 | syntax error |
| E102 | invalid token: unterminated string, bad regex literal... |
| E201 | unsupported syntax: `@load`, `@namespace`, indirect calls, `$$1` |
| E202 | arrays of arrays `a[i][j]` (use `a[i,j]`) |
| E301 | call to an unknown function |
| E302 | wrong number of arguments to a builtin function |
| E303 | a variable used both as an array and as a scalar |
| E305 | assignment to a read-only variable or field |
| W101 | gawk variable with no meaning in zawk (`FIELDWIDTHS`, `FPAT`, `RT`) |
| W103 | `f` or `r` used as a name (clashes with f-strings and raw strings) |
| W105 | `$FI[...]` without `-H` |
| W116 | `$NF-1` is the last field minus 1, not `$(NF-1)` |
| W117 | field compared with a numeric string (`$3 > "100"`): a string comparison |
| W118 | `printf $0`: a `%` in the data breaks it |
| W119 | `FS` assigned in a main rule: applies from the next record only |
| W201 | variable read but never set |
| W202 / W203 | `printf` format with more / fewer arguments than given |
| W206 | variable set but never read (typo: `totl += $1` ... `print total`) |
| W207 | function defined but never called |
| W301 / W302 | `NR` and END aggregation semantics in parallel mode |

## `zawk fmt`

```shell
zawk fmt demo.awk              # print the formatted program
zawk fmt -w *.awk              # write files back
zawk fmt --check *.awk         # exit 1 if a file is not formatted (CI)
zawk fmt --indent 2 demo.awk   # or --tabs
```

Blocks are indented, one statement per line, spaces are normalized; comments, blank lines and literals are
kept. The result is checked to parse to the same program as the original.

## `zawk llm`

Builds a prompt with your data's schema and sample, the zawk language notes and your question. See
[AI integration](./tutorials/ai.md).

```shell
zawk llm -i csv -H demo.csv 'how many adults?' | llm -x
```

## Debugging

```shell
zawk --dump-cfg '...'          # untyped control-flow graph
zawk --dump-bytecode '...'     # bytecode
zawk -Binterp '...'            # bytecode interpreter instead of the Cranelift JIT
```

Inside a program, `var_dump(x)` prints any value or array to stderr, and `log_debug`/`log_info`/`log_warn`/
`log_error` write logs controlled by `RUST_LOG`.
