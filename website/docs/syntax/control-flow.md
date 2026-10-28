---
title: Variables, Loops & Fields
sidebar_label: Variables, loops & fields
---

# Variables, Loops & Fields

## Local variables

In AWK, every variable is global, and the only way to get a local is the "extra function parameter" idiom.
zawk adds `local`:

```awk
{
    local total = 0, seen[]
    for (i = 1; i <= NF; i++) {
        if ($i in seen) continue
        seen[$i]
        total += $i
    }
    print total
}
```

- `local a = expr` declares `a` for the rest of the rule (or function body), including nested blocks. It
  shadows a global `a`, which other rules still see.
- A local is uninitialized again on each run of the rule: `{ local n; n++; print n }` prints `1` for every record.
- `local arr[]` declares an empty array. Several can be declared at once: `local i, s = "", seen[]`.
- `local x = x + 1` reads the outer `x`.
- In a function, locals are new on each call, like the extra-parameter idiom.
- Special variables (`NR`, `FS`, `SUBSEP`...) and function names cannot be local.

## Key-value iteration

Like Python's `for k, v in d.items()`: `for (k, v in arr) body` is the same as
`for (k in arr) { v = arr[k]; body }`.

```shell
zawk '{ c[$1]++ } END { for (k, v in c) print k, v }' words.txt
```

On an array made by `split`, it works like Python's `enumerate`:

```awk
BEGIN {
    n = split("red green blue", colors)
    for (i, name in colors) print i, name
}
```

`arr` must be an array variable: `for (k, v in f())` is a syntax error, assign the result first
(`a = f(); for (k, v in a)`). The classic `for (k in f())` does accept a function call.

The iteration order is unspecified unless `PROCINFO["sorted_in"]` is set to one of gawk's predefined orders
(`"@ind_str_asc"`, `"@val_num_desc"`, ...):

```awk
END {
    PROCINFO["sorted_in"] = "@val_num_desc"
    for (word, n in count) print n, word
}
```

## Range loops

`..` (exclusive) and `..=` (inclusive) are range operators in a `for (i in start..end)` header:

```shell
$ zawk 'BEGIN { for (i in 1..4) printf "%d ", i; print ""; for (i in 1..=4) printf "%d ", i; print "" }'
1 2 3
1 2 3 4
```

Range bounds are integers. Outside a `for` header, `1..3` keeps its AWK meaning (`1.` concatenated with `.3`).

## Negative field indexes

Like Python's `xs[-1]`: `$(-k)` is `$(NF + 1 - k)`, so `$(-1)` is `$NF` and `$(-2)` is `$(NF-1)`.

```shell
$ echo "a b c" | zawk '{ print $(-1), $(-2) }'
c b
```

- Write the index in parentheses: `$-1` is a syntax error.
- Reading before the first field gives `""`; assigning there is a fatal error.
- Only a literal index counts from the end. A negative index computed at runtime (e.g. `$(NF-2)` on a one-field
  record) is a fatal error, as in gawk, instead of silently reading another field.

## `assert`

`assert(cond, msg)` fails fast: when `cond` is false, `msg` is printed to stderr and zawk exits at once with
code `2`. Unlike `exit 2`, the `END` block is **not** run.

```shell
zawk -i csv '{ assert(NF == 5, f"line {NR}: expect 5 fields, got {NF}") } { ... }' data.csv
```

## Field list shorthand

A command-line program that is only a comma-separated list of two or more fields prints them:

```shell
zawk '$1,$3' file           # zawk '{ print $1, $3 }'
zawk '/ERROR/' app.log      # print matching lines
```

A single field such as `zawk '$2'` keeps the standard AWK meaning: print records whose `$2` is non-empty and
non-zero. Use `--posix` to disable all command-line sugar.

## Functions returning arrays

User-defined and stdlib functions can return arrays, and a call can be indexed directly:

```awk
function keys_of(arr,    k, out, n) {
    for (k in arr) out[++n] = k
    return out
}

{ print url($1)["host"], pairs(url($1)["query"], "&")["id"] }
```

## Multidimensional arrays

POSIX-style multidimensional arrays are supported: `a[i, j]`, `(i, j) in a`, `delete a[i, j]` and `SUBSEP`.
True arrays of arrays (`a[i][j]`) are not.

## Includes

`@include "lib"` (or `--include lib` on the command line) loads an AWK library file once, searched in `AWKPATH`
(default `.:/usr/local/share/awk`), trying `lib` then `lib.awk`.

## Running Markdown

`-f` accepts a Markdown file and runs its first ` ```awk ` (or `zawk`/`gawk`) code block, so documentation and
LLM answers are runnable as is:

```shell
zawk -f notes.md data.txt
zawk -i csv -H "$(zawk llm -i csv -H demo.csv 'how many adults?' | llm)" demo.csv
```
