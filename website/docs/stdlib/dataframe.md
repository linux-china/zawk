---
title: DataFrames
sidebar_label: DataFrame
---

# DataFrames

AWK is at its best streaming records through the main loop. Some work happens after the stream,
in `END`: grouping on several keys, joining two files, sorting on several columns, pivoting.
With arrays alone this means nested `for (k in a)` loops, `SUBSEP` keys and hand-written sorts.
zawk's `df_*` functions do it on in-memory tables.

```awk
END {
    d = df_read("orders.csv")
    g = df_group(df_filter(d, "status == 'paid'"), "city", "sum(amount) as total, count() as n")
    df_print(df_head(df_sort(g, "total desc"), 10), "table")
}
```

DataFrames add **no syntax and no type** to the language: a DataFrame is a *string handle* such as
`"df#1"`, so any awk parser reads the program (gawk would report an unknown function, not a syntax
error), and the existing programs, their output and their speed are unchanged.

## Handles

```awk
d = df_read("orders.csv")      # d is "df#1"
print typeof(d)                # "dataframe" while the table exists, "string" after df_free(d)
df_free(d)                     # optional: tables are released at exit
```

* A handle is a string: store it in a variable or an array, pass it to and return it from your
  functions, print it.
* Every `df_*` function that transforms a table returns a **new handle**; the input table is not
  changed. Only `df_append`/`df_append_record` add rows to a table in place.
* Rows and column numbers start at **1**, as `$1` and `NR` do.
* A user-defined function named like a `df_*` function takes precedence, as for every zawk stdlib
  function, so existing scripts keep working.
* Errors are those of the other stdlib functions: a warning on stderr (`ERRNO` is set) and an
  empty result, `""` or `0`. `try ... else` catches them:
  `n = try df_nrow(d) else -1`. An unknown column suggests a close name:
  `df_filter: unknown column "agee" (did you mean "age"?)`.
* Do not use `f` or `r` as variable names (they are the f-string and raw-string prefixes); the
  examples use `d`, `g`, `t`.

## Building tables

| Function | Description |
|---|---|
| `df_read(path[, format[, options]])` | Load a file or URL: csv, tsv, json, jsonl, parquet, avro, xlsx (`book.xlsx#Sheet2`), markdown, `sqlite://app.db#table`, `mysql://`, `postgres://`, `http(s)://`, `s3://`, compressed files. The format comes from the extension (or the content) unless given. |
| `df_new("name,age:int,city")` | An empty table with these columns; `:TYPE` declares a type, as the header suffixes of `-o parquet` do. |
| `df_append(d, v1, v2, ...)` | Append a row; returns the row count. |
| `df_append_record(d)` | Append the fields `$1..$NF` of the current record. An empty table first takes the column names of the header (`FI`, with `-H`), or uses the record as its header without `-H` (so with several input files, the header line of the second file is appended as a row, as in plain awk). |
| `df_from_array(arr[, "key,value"])` | A two-column table of the keys and values of an array, sorted by key (the two names must differ). |

`df_read` options are `;`-separated: `infer_types=1` (CSV/TSV text becomes int, double or bool
columns where every value fits), `header=0` (no header line, also for a markdown table: columns `c1`, `c2`, ...), `limit=N`
(read N rows), `columns=a,b` (keep these columns).

CSV and TSV (by extension, by the format argument, or sniffed from the content of `-` or a file
without a known extension) are split as `-i csv -H` / `-i tsv -H` split them: bytes that are not
UTF-8 are kept, and in a quoted CSV field a `"` that is not followed by `,`, a line end or another
`"` is a literal quote (`"say "hi" now",x` reads `say "hi" now` and `x`).

```awk
d = df_read("logs.parquet")
d = df_read("book.xlsx#Sheet2")
d = df_read("sqlite://app.db#users")
d = df_read("postgres://postgres:123456@127.0.0.1:15432/demo_db#people")
d = df_read("s3://bucket/data.csv.gz", "", "infer_types=1;columns=id,amount")
```

By default CSV and TSV values are kept as text, exactly as awk reads the fields: they are
*strnums*, so `"10"` compares numerically with `9`. Parquet, Avro, JSON, Excel and database
values keep their types. CSV/TSV text that is not valid UTF-8 is kept byte for byte.

**Collecting rows in the main loop** is the awk way to build a table:

```awk
BEGIN { d = df_new("city,amount:float") }
$FI["status"] == "paid" { df_append(d, $FI["city"], $FI["amount"]) }
END {
    g = df_group(d, "city", "sum(amount) as total, count() as n")
    df_print(df_sort(g, "total desc"))
}
```

## Querying and transforming

| Function | Description |
|---|---|
| `df_select(d, "a, b as x")` | Columns in that order (`as` renames). |
| `df_drop(d, "a, b")` | Without these columns. |
| `df_filter(d, expr[, name, value, ...])` | The rows for which the expression is true. |
| `df_mutate(d, "x = a * 2, y = upper(name)")` | Add or replace columns; an assignment may use the columns assigned before it. `$2 = ...` (or `2 = ...`) replaces the 2nd column, keeping its name. |
| `df_sort(d, "age desc, name")` | Stable sort; see below. |
| `df_head(d[, n])`, `df_tail(d[, n])` | The first / last n rows (10 by default). |
| `df_slice(d, from[, n])` | n rows from row `from`. |
| `df_distinct(d[, "cols"])` | The first row of each distinct value of the columns (all by default), in order, as `uniq(arr, "keep")`. |
| `df_group(d, "keys", "aggs")` | Group and aggregate; see below. |
| `df_join(a, b, "on"[, how])` | `inner` (default), `left`, `right`, `full`, `semi`, `anti`; `on` is `id`, `user_id=id` or `a,b`. |
| `df_concat(a, b, ...)` | Rows one after the other, columns matched by name. |
| `df_pivot(d, "row", "col", "agg(val)")` | A wide table: one column per value of `col`. |
| `df_melt(d, "id_cols"[, var[, value]])` | A long table: one row per non-id cell. |

Column names in lists are plain names, `"quoted names"` or `$"quoted names"` for names with
spaces, or column numbers (`$3`).

### Expressions

`df_filter`, `df_mutate` and the aggregates of `df_group` take **expressions as strings**, in a
subset of awk:

* columns: `age`, `$"first name"`, `$3`;
* literals: `30`, `1.5`, `"text"` or `'text'` (handy inside a double-quoted awk string), `/regex/`;
* operators: `?:` `||` `&&` `in` `~` `!~` `<` `<=` `==` `!=` `>` `>=`, concatenation,
  `+ - * / %`, unary `! - +`, `^`; `city in ("Paris", "Rome")`;
* functions: `length substr index tolower toupper lower upper trim ltrim rtrim int abs round
  floor ceil sqrt exp log starts_with ends_with contains is_null coalesce min max number string`.

Comparisons follow awk's rules: a column read from text compares numerically when both sides look
like numbers (`age > 30`), and as a string otherwise (`city == "Paris"`); an empty value is `""`
(`"" < 1` is true, as in awk). Regexes use zawk's regex dialect.

**Parameters** pass awk values into an expression without building its text: name/value pairs
after the expression, referenced as `@name`. Never concatenate values into an expression (it is
the SQL injection problem and quoting is painful):

```awk
df_filter(d, "age > @min && city == @city", "min", min_age, "city", $2)
```

### Sorting

Each key is `asc` (the default) or `desc`. Values compare as awk compares them (numbers
numerically, other text byte by byte); `num`/`str`, or a `PROCINFO["sorted_in"]` order name,
force a comparison: `df_sort(d, "id @val_num_asc, name desc")`. In every mode, nulls and missing
values (`""`, the `--null` text) sort first in ascending order; `desc` reverses the whole order, so
they sort last in descending order (`df_head(df_sort(d, "amount desc"), 3)` gives the three
largest amounts). The sort is stable.

### Grouping

```awk
df_group(d, "city, year", "sum(amount) as total, mean(amount), quantile(latency, 0.99) as p99, count() as n")
```

Aggregates: `sum mean (avg) min max median quantile(x, q) var stddev mode count count_distinct
first last concat(x[, sep])`. Their argument is an expression (`sum(price * qty)`). Without `as`,
the name is `function_column` (`mean_amount`), or `count`.

* Missing values (null, `""`, the `--null` text) are skipped, as `_sum`/`_mean` skip them, and the
  statistics are computed by the same code as the array functions: `median(x)` of a group equals
  `_median(a)` of an array holding the same values.
* `count()` counts rows, `count(x)` the values of `x` that are not missing.
* A group in which every value of `x` is missing gets **null** (printed as the `--null` text, empty
  by default), not 0, for `sum mean min max median quantile var stddev mode concat`, as SQL aggregates
  do: a null total is "no data", a 0 total is a real 0. `count(x)` and
  `count_distinct(x)` are 0. (The array functions `_sum`/`_max`/... still return 0 for an array
  without values, since awk has no null.)
* `mode(x)` of numbers is the number `_mode` gives (the smallest on a tie); of text, such as a
  category column, the most frequent text (the smallest as awk compares on a tie), so
  `mode(city)` is a city, not 0.
* `min`/`max` compare numbers numerically, and text (dates, names) as awk compares strings.
* Groups come in the order of their first row; sort them with `df_sort`. Keys `""` aggregate the
  whole table into one row.

## Back to awk arrays

| Function | Returns |
|---|---|
| `df_nrow(d)` / `df_ncol(d)` | Number of rows / columns. |
| `df_columns(d, arr)` | Column names in `arr[1..n]`; returns n. |
| `df_get(d, row, col)` | One cell (`col` is a name or a 1-based number). |
| `df_row(d, i, arr)` | Row `i` in `arr[column name]`. |
| `df_col(d, "age", arr)` | A column in `arr[1..n]`, ready for `_median(arr)` and the other array functions. |
| `df_to_map(d, "key", "val", arr)` | `arr[key] = val` for each row. |
| `df_schema(d)` | `"name:string,age:int64"`. A name with a `,`, `:` or `"` is quoted as a CSV field: `"a,b":string`. |

Walking the rows is a plain awk loop:

```awk
for (i = 1; i <= df_nrow(d); i++) {
    df_row(d, i, row)
    print row["name"], row["age"]
}
```

## Output

| Function | Description |
|---|---|
| `df_print(d[, format])` | Print to stdout, through the same output as `print`. |
| `df_write(d, path[, format])` | Write a file (`.gz` compresses) or an `s3://` object. |
| `df_describe(d)` | A summary of each column (type, count, nulls, distinct, min, max, mean, std). |
| `df_to_csv(d)` / `df_to_json(d)` | The table as CSV text / a JSON array. |

`df_print` shares stdout with `print`: their output interleaves in program order, and `fflush`
and `close` behave as for `print`. Without a format it follows the output of the run:

* without `-o`: a header line, then one line per row, fields separated by `OFS` and rows ended by
  `ORS`, as `print` writes them, so the output can feed another awk;
* with `-o csv`, `-o tsv`, `-o jsonl`, `-o markdown`, `-o table`, `-o parquet`, ...: the table
  in that format. stdout is then a single table whose first line is its header, so `df_print`
  writes the header only when nothing was written to stdout before; after a `print` or another
  `df_print`, it writes its rows under the header already there (with a warning when its columns
  differ from those of an earlier `df_print`). Several `df_print` calls with the same columns
  therefore make one table;
* with a format argument (`csv`, `tsv`, `json`, `jsonl`, `markdown`, `table`): that format, for a
  run without `-o` or with `-o csv` / `-o tsv` (written as text). The format of `-o` itself is the
  output of the run, as without an argument; another format is ignored, with a warning, by
  `-o jsonl/markdown/table/parquet/avro/xlsx`, whose writer reads stdout as TSV: use `df_write`
  for a second format.

`df_write` writes csv, tsv, json, jsonl, markdown, table, parquet, avro or xlsx (from the
extension by default) with the same writers as `-o`: typed columns get the `name:TYPE` header
suffixes, so `df_write(d, "out.parquet")` writes the file `-o parquet` writes for the same rows.
`df_write(d, "-")` and `df_write(d, "/dev/stdout")` (CSV by default) write to stdout after the
output `print` wrote before (not with `-o jsonl/markdown/table/parquet/avro/xlsx`: use `df_print`),
and `"/dev/stderr"` writes to stderr. Before writing a file, `df_write` flushes the output of
`print` to files and pipes (as `system` does); a file still open with `print > "out.csv"` is not
overwritten: `close("out.csv")` it first.
Numbers are printed with `OFMT` by `df_print` (as `print` does), and in full precision in typed
files.

## Examples

```awk
# Top 10 cities by revenue
END {
    d = df_read("orders.csv")
    g = df_group(df_filter(d, "status == 'paid'"), "city", "sum(amount) as total, count() as n")
    df_print(df_head(df_sort(g, "total desc"), 10), "table")
}

# Join two files (the classic NR == FNR idiom still works)
BEGIN {
    u = df_read("users.csv"); o = df_read("orders.csv")
    df_print(df_join(o, u, "user_id=id", "left"))
}

# Collect in the main loop, pivot in END
BEGIN { d = df_new("day,level,n:int") }
{ df_append(d, substr($1, 1, 10), $3, 1) }
END { df_print(df_pivot(d, "day", "level", "sum(n)"), "markdown") }

# Back to awk arrays
END {
    df_col(df_read("lat.csv"), "latency", lat)
    print _median(lat), _quantile(lat, 0.99)
}

# Convert formats
BEGIN { df_write(df_read("events.jsonl"), "events.parquet") }
```

## Parallel runs and memory

* A table lives in memory: `df_read` loads the whole input. Use `limit=` and `columns=` to load
  less, and `df_free` tables built in a loop.
* Every intermediate table stays in the registry until `df_free`: `df_nrow(df_filter(d, ...))` in a
  main rule leaves one table per record. `zawk lint` reports such a nested call as **W130**; keep the
  handle and free it (`t = df_filter(d, ...); n = df_nrow(t); df_free(t)`), or do the work in `END`.
* The tables are shared by the threads of a parallel run (`-p`). Rows that `df_append` adds in the
  main rules come from all the workers, in an unspecified order: `zawk lint -pr` and a `-p` run
  report it as **W307**. Sort the table in `END`, or run serially (`-p0`).
* Without `-H`, `df_append_record` in the main rules of a parallel run cannot take the first record
  as the header of a table with no columns: the record that comes first is that of the worker first
  to append, not the first line of the input. The call is then an error (a warning, the record is
  not added); use `-H`, or declare the columns with `df_new("a,b,...")`. A program that calls
  `df_*` functions in its main rules is never parallelized automatically.
* A program that does not call a `df_*` function pays nothing: the registry of tables is created on
  first use.

## Compatibility with awk

* No new keyword, operator or syntax; `zawk fmt` is unchanged.
* `df_*` functions never change `$0`, `NF`, `NR` or `FI`; `df_row` and `df_col` fill the array you
  pass.
* Values follow awk: 1-based rows and columns, strnum comparisons, `CONVFMT`/`OFMT` number
  formatting, empty values skipped by aggregates as by `_sum`, byte order for strings.
* Database BLOBs are read as empty strings, as everywhere in zawk.
* Not supported: SQL over DataFrames, and Polars/Arrow integration.
