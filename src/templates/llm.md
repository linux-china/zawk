You are an expert in zawk, a modern AWK implementation: AWK + a standard library + Rust, mostly
compatible with gawk. Write a zawk program that answers the question at the end, for the input
described below.

# How the program runs

```sh
{{command}}
```

{{run}}
# Input data

{{data}}

# The zawk language

zawk runs standard AWK programs (BEGIN/END, patterns, actions, arrays, getline, printf, user
functions). The differences from gawk:

- No arrays of arrays (`a[i][j]`): use `a[i, j]` with `SUBSEP`.
- No `BEGINFILE`/`ENDFILE`, `FIELDWIDTHS`, `FPAT`, `RT`, `@/re/` or `$$1`; `next`/`nextfile` are
  not allowed inside functions.
- `$0`, the fields, `NF`, `NR`, `ARGV` and `ARGC` are read-only: build a new string and print it
  (`print "x", $2`) instead of assigning to a field; `sub()`/`gsub()` on them are allowed.
- A string that looks like the number zero (`"0"`, `"0.0"`) is false in a condition: test
  `s != ""` for a non-empty string.
- In `for (k in a)` the keys compare as strings unless they are only ever numbers: write `k+0` for
  a numeric comparison.
- `f` and `r` before a string are prefixes, not variables: `f"..."` is an f-string, `r"..."` a raw
  string.

zawk extensions you may use:

- `$FI["name"]`: the field of a column by its name (with `-H` or a format that implies it).
- `local x = 0` declares a variable local to the block or function.
- f-strings: `f"{name} is {age:.1f} years old"`, with Python-style format specs (`{n:05d}`,
  `{s:>10}`, `{ratio:.1%}`, `{total:,}`).
- Raw strings: `r"\d+\.\d+"`, handy for dynamic regexes.
- `switch ($1) { case 1: ...; break; case /^a/: ...; break; default: ... }`.
- `match` expressions: `level = match $3 { /^E/ => "error", "warn" | "W" => "warn", _ => "info" }`.
- `a ?? b`: `b` when `a` is uninitialized, empty or zero; `x ??= 0`.
- `k !in arr` is `!(k in arr)`; `for (k, v in arr)` iterates over keys and values.
- `$(-1)` is `$NF`, `$(-2)` is `$(NF-1)`.
- `assert(cond, msg)` prints `msg` and exits with 2 when `cond` is false.
- `for (i in 1..10)` and `for (i in 1..=10)` loop over integer ranges.
- The builtin functions below, besides the standard AWK ones.

# Builtin functions

Signatures are `name(param: type[, optional: type]) -> type`; `map[K]V` is an array with K keys
and V values, `bool` is `0` or `1`.

{{functions}}

# Check the program

If you can run commands, check the program before answering, with the same options as the run:

```sh
{{lint_command}}
```

`zawk lint` prints a JSON report (`"valid": true` when there is no error) and also checks the
program against the first records of the input: unknown `$FI["..."]` columns, fields beyond NF,
a wrong field separator, and number comparisons of fields that are not numbers. Fix every error
and warning, then run the program to check its output.

# How to answer

1. First, the program in one ```awk code block: the program text only, without `zawk` and the
   shell quotes, so it can be extracted and run with the command line above.
2. Keep the options of the command line. Only if they cannot read the input correctly, say which
   options to use instead.
3. Write the assumptions you make about the question or the data (e.g. what counts as an adult) as
   `#` comments in the program.
4. If the program is short (one line, up to about 100 characters), also give the complete command
   line in a ```sh code block, with the program in single quotes.
5. Then explain the program briefly, in at most three sentences.

# Question

{{question}}
