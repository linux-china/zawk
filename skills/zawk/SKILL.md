---
name: zawk
description: >
  Process text and structured data on the command line with zawk, a fast AWK (gawk mostly compatible) written in Rust
  with a standard library of ~190 functions. Use it to filter, transform, aggregate and convert CSV/TSV, JSON,
  JSON Lines, Parquet, Excel, Markdown tables, database tables and logs (local files, stdin, http(s):// or s3://,
  gz/zst/bz2/xz/lz4 transparently); to output CSV/TSV/JSON Lines/Markdown/terminal tables/Parquet/Excel; and to call JSON/XML/HTML parsers, date/time, crypto/JWT, encode/hash, ID
  generators, HTTP, SQLite/MySQL/PostgreSQL, KV (SQLite/Redis/NATS), S3, NATS/MQTT and LLM decisions (jev) per record.
  Prefer it over awk/gawk, sed+cut pipelines or one-off Python scripts for line-oriented data processing.
allowed-tools: Bash
---

# zawk

zawk is AWK + stdlib + Rust: the AWK language you already know, compiled with Cranelift (usually 2-8x faster than
gawk), plus built-in readers/writers for structured data and a standard library for text, JSON, dates, crypto,
network and databases. It is a single binary: `zawk 'program' [file ...]`.

Use zawk when the task is "scan records, filter/transform/aggregate, print a result", especially when:

- the input is CSV/TSV (with quoting), JSON, JSON Lines, Parquet, Excel, a Markdown table, a database table or a log
- the output should be CSV/TSV, JSON Lines, a Markdown table (for a reply/PR/issue), a Parquet file or an Excel file
- each record needs a function AWK lacks: `trim`, `json_value`, `strftime`/`mktime`, `digest`, `uuid`, `url` ...
- each record triggers a side effect: an HTTP call, a SQL insert, a KV write, a message, an LLM yes/no decision

Use something else for: a single big JSON document (jq), SQL joins/window functions over many files (DuckDB),
in-place file edits (sed -i).

## Discover functions: `zawk functions`

Don't guess function names or argument orders: ask zawk. It is cheaper than reading the docs and never out of date.

```shell
zawk functions                 # all functions, grouped by category: signature, summary, example
zawk functions json            # functions whose name contains "json"
zawk functions Crypto          # one category (Text, Parser, Array, Math, Date/Time, JSON, Network, Database, ...)
zawk functions --json trim     # machine readable, one function per line:
# {"name":"trim","category":"Text","signature":"trim(s: str[, chars: str]) -> str",
#  "params":[{"name":"s","type":"str","optional":false},{"name":"chars","type":"str","optional":true}],
#  "returns":"str","summary":"Remove whitespace, or any of chars, from both ends of s.","example":"trim(\"[hello]\", \"[]\")"}
```

Signatures: parameters after `[` are optional; types are `str`, `int`, `float`, `num`, `bool` (0/1), `any`, `array`,
`map[K]V` (an array returned or expected by the function) and `void`.

## Check a program before running it: `zawk lint`

`zawk lint` parses and type checks a program without reading any input, and prints a JSON report. Run it on
non-trivial programs, fix every `error`, then run the program.

```shell
zawk lint '{ print $1 }'               # program text (sugar syntax like '$1,$2' is expanded, as for a run)
zawk lint -f demo.awk                  # program files (-f and -e can be repeated, as for a run)
echo 'BEGIN { print 1 }' | zawk lint   # program from stdin
zawk lint -v n=10 -f top.awk           # n is set on the command line: not reported as never set
zawk lint -pr -f sum.awk               # also report code that changes meaning with -pr / -pf
zawk lint -H -f report.awk             # the run uses -H (or -i jsonl/parquet/markdown): $FI[...] is filled
zawk lint -i csv -H '{ print $FI["nmae"] }' users.csv   # with the input file: also checked against its first records
zawk --lint -i csv -H -f report.awk users.csv            # lint, then run when there is no error (report on stderr)
```

Pass the input file to `zawk lint` whenever you can: the program is checked against the header and the first 100
records (`--sample-rows N`), which finds misspelled column names (W126, with a "did you mean" hint), fields beyond `NF`
(W127), a wrong field separator (W128) and number comparisons on fields that are not numbers (W129). A misspelled
column is silently `""` when the program runs, so this is the only place it is reported.

```json
{
  "valid": false,
  "errors": 1,
  "warnings": 0,
  "diagnostics": [
    {
      "code": "E301",
      "severity": "error",
      "message": "Call to unknown function \"foo\"",
      "file": "demo.awk",
      "line": 3,
      "column": 9,
      "source": "  print foo(1)",
      "caret": "        ^"
    }
  ]
}
```

The exit status is 0 when `valid` is true (warnings allowed), 1 otherwise. `line` and `column` are 1-based (columns
count characters); `file` is null for program text, and all location fields are absent when an error has no location.

| Code | Meaning                                                                          |
|------|----------------------------------------------------------------------------------|
| E001 | a program file cannot be read                                                    |
| E002 | the input file passed to `zawk lint` cannot be read                              |
| E101 | syntax error: unexpected token or end of program                                 |
| E102 | invalid token: unterminated string, bad regex literal, unexpected character, ... |
| E201 | unsupported syntax: `@include` not alone on its line, `@load`, `@namespace`, indirect calls, `$$1`/`$++i` (use `$($1)`) |
| E202 | unsupported feature: arrays of arrays `a[i][j]` (use `a[i,j]`)                   |
| E203 | gawk function not available in zawk (e.g. `patsplit`)                           |
| E300 | other compile error                                                              |
| E301 | call to an unknown function                                                      |
| E302 | wrong number of arguments to a builtin function                                  |
| E303 | a variable is used both as an array and as a scalar                              |
| E305 | assignment to a read-only variable (`ARGC`, `ARGV`, `FILENAME`, `NR`, `NF`, `FI`) or a field (`$0`, `$1`, `$FI["col"]`) |
| E306 | assigning to something that is not a variable, array element or field: `substr(s, 1) = "a"`, `gsub(re, r, substr(s, 1))`, `x++` on a literal |
| E307 | a string constant used as a regex is invalid: `s ~ "["`, `gsub("+", "x")`, `split(s, a, "[a")` (the run would fail) |
| E900 | internal compiler error                                                          |
| W101 | gawk variable with no special meaning in zawk (`FIELDWIDTHS`, `FPAT`, `RT`, ...) |
| W102 | unknown escape sequence in a string or regex (`"a\.b"` is `a.b`): write `\\.` for a literal dot |
| W103 | `f` or `r` used as a variable or function name (clashes with f-strings `f"..."` and raw strings `r"..."`/`r#"..."#`) |
| W105 | `$FI[...]` without `-H` (or `-i jsonl\|parquet\|markdown`): `FI` is empty, so `$FI["col"]` is `""`; pass `-H` to lint too |
| W108 | array key from `for (k in a)` compared with a number: a string comparison in gawk, numeric in zawk; write `k+0` |
| W114 | `print $1 > 10`: `>` after print redirects to the file `10`; write `print ($1 > 10)` to print a comparison |
| W115 | assignment in a condition (`if (x = 1)`): use `==`, or compare the assigned value explicitly |
| W116 | `$NF-1` is the last field minus 1, not `$(NF-1)`                              |
| W117 | field compared with a numeric string (`$3 > "100"`): a string comparison         |
| W118 | `printf $0` (format reads a field, no arguments): a `%` in the data breaks it; use `printf "%s", $0` |
| W119 | `FS` assigned in a main rule: applies from the next record only; set it in BEGIN or with `-F` |
| W120 | string constant as a condition (`if ("0")`): false in zawk if it looks like 0, true in gawk |
| W121 | string constant after `$` (`$"age"`): converted to a number, so it is `$0`, not a column; use `$FI["age"]` with `-H` |
| W126 | (input file given; severity `error` despite the W) unknown column in `$FI["col"]`: always `""`; with a "did you mean" hint |
| W127 | (input file given) `$N` beyond the number of fields of the sampled records: always empty |
| W128 | (input file given) the program reads `$2`... but with this `FS` every sampled record has one field: did you mean `-F,` or `-i csv`? |
| W129 | (input file given) field compared with a number, but some sampled values are not numbers: those records compare as strings |
| W201 | variable read but never set (pass `-v name=...` or declare it with `# @var name`) |
| W202 | `printf`/`sprintf` format uses more arguments than given (missing ones are empty)|
| W203 | `printf`/`sprintf` format uses fewer arguments than given (extra ones ignored)   |
| W204 | user function called with more arguments than it declares                        |
| W205 | `index(/re/, s)`: a regex literal there means `$0 ~ /re/` (use a string, or `match`) |
| W206 | variable set but never read (the other side of a typo: `totl += $1` ... `print total`) |
| W207 | function defined but never called (not reported for a program of only functions) |
| W208 | string bound in `for (i in start..end)` (`"a".."c"`): bounds are integers, a string is 0, so the loop may run no iteration |
| W218 | a field, `NF` or `$0` read in BEGIN, before any input is read: use `NR == 1 { ... }` or `getline` first |
| W301 | with `-p`: `NR` (and `FNR` with `-pr`) counts per worker, so `NR==1` hits once per worker |
| W302 | with `-p`: scalar set with `=` in the main loop and read in END is aggregated over workers (numbers summed, strings taken from one worker), not its last value; collect it in `PREPARE { v[PID] = x }` |

Only the first error is reported: fix it and lint again.

## Command line

```shell
zawk [options] 'program' [file ...]
zawk [options] -f script.awk [file ...]
```

| Option | Meaning |
|--------|---------|
| `-F fs` | Field separator, as in awk (`-F,`, `-F'\t'`) |
| `-v var=value` | Set a variable before BEGIN (repeatable) |
| `-f file` / `-e text` | Program from a file (or URL) / from text (repeatable, concatenated) |
| `--include lib` / `@include "lib"` | Load an awk library file once, searched in `AWKPATH` (default `.:/usr/local/share/awk`), `lib` then `lib.awk`; remote files are not supported |
| `-E file` | Like `-f`, all following arguments are input files: `#!/usr/bin/env -S zawk -E` |
| `-i csv\|tsv\|json\|jsonl\|parquet\|markdown\|xlsx` | Input format (NOT gawk's `-i include`). json, jsonl, parquet and markdown imply `-H`; json is the default for `.json` files, jsonl for `.jsonl`/`.ndjson`, parquet for `.parquet`, xlsx for `.xlsx` files (needs `-H` for a header row) |
| `-H` | First line is a header: `$FI["col"]` is the field of column `col`; the header is not a record; with several files, each file's first line is its header |
| `-o csv\|tsv\|jsonl\|markdown\|table\|parquet\|xlsx` | Output format of `print` to stdout (`md` = markdown, `ndjson` = jsonl); except csv/tsv, the first printed record is the header |
| `--out-file FILE` | Write stdout to FILE (required for `-o parquet`/`-o xlsx` unless stdout is redirected) |
| `--infer-types` | With `-o jsonl/parquet`, columns without a type suffix are `:auto`: numbers and `true`/`false` keep their type (`007` stays a string) |
| `--summary` | With `-o table/markdown`: after the table, the row count and per-column type, non-empty, distinct, min, max, sum, mean |
| `--null TEXT` | Read NULLs of database, JSON Lines and Parquet input as TEXT (e.g. `--null '\N'`) instead of `""`: `isnull($2)` tests it; `-o tsv` writes it as `\N`, `-o jsonl/parquet` as null, query parameters bind it as NULL |
| `--stats` | On exit, print `{"input_records":..,"output_records":..,"rejected_records":..,"elapsed_secs":..,"records_per_sec":..,"exit_code":..}` on stderr |
| `--lint` / `--deny-warnings` | Lint before running (report on stderr); don't run on errors (or on warnings too) |
| `-p r -j N` | Run in parallel over records with N workers (simple scripts on large files) |
| `--posix` | Disable zawk's command line sugar |

Input files may be local paths, `-` (stdin), `https://...` URLs or `s3://bucket/key`; compressed input is detected
and decompressed automatically. A JSON document (`-i json`, the default for `.json` files) is read as JSON Lines:
each element of a top-level array is a record (an object is one record); select the records with a JSONPath after
`#`: `'data.json#$.data'`, `'https://api.example.com/users#$.items'`. A NATS subject (`'nats://host:4222/logs.>'`, wildcards allowed) as the only input
reads its messages until Ctrl+C, then runs END: each message is an input file (`-i`/`-H` apply to its payload), with
`MSG_SUBJECT`, `MSG_HEADERS["name"]` and `MSG_BODY`. A Kafka topic (`'kafka://host:9092/topic'`, with `?group=name` to
share it and resume, and `?offset=earliest` to start at the oldest message; needs Kafka 4.0 or newer) is read the same
way, MSG_SUBJECT being the topic. A database URL reads a table or a query as CSV with a header
line (`FI` from the column names, no `-i`/`-H` needed): `'sqlite://./app.db#user'`, `'mysql://u:p@host:3306/db#people'`,
`'postgresql://u:p@host:5432/db#select name, age from people'` (after `#`: a table name, or SQL when it contains
spaces; NULL values are `""` (or the `--null` text) and BLOBs are `""`; for a large table, use SQL with `LIMIT` when only the first rows are needed). `.env` in the current directory is loaded as environment variables.

Command line sugar (not in `--posix` mode): `zawk '$1,$3' file` prints fields 1 and 3, `zawk '/re/' file` prints
matching lines.

## Subcommands

```shell
zawk inspect data.parquet                  # format, the command to read it, schema, sample rows, statistics
zawk inspect -n 5 --max-rows 10000 big.csv # 5 sample rows; statistics on the first 10000 rows only (fast)
zawk inspect -o md book.xlsx#Sheet2        # the report as Markdown (default: terminal tables); lists the sheets
zawk inspect 'sqlite://app.db#users'       # also tables/queries of SQLite, MySQL, PostgreSQL; -i csv|tsv|json|jsonl|parquet|xlsx
zawk llm -i csv -H family.csv 'how many adults?' | llm   # a prompt (input samples + zawk rules) for an LLM to write the program
zawk dump --prometheus http://localhost:8080/metrics     # Prometheus/OpenMetrics text -> CSV (name, labels, type, value1, value2)
zawk functions json                        # stdlib signatures (see below); zawk lint: see above
zawk fmt prog.awk                          # format a program (-w to rewrite the file, --check in CI)
zawk test lib/                             # run the test_* functions of .awk files
```

## Structured input and output

```shell
# CSV with header: columns by name
zawk -i csv -H '$FI["age"] > 40 { print $FI["name"], $FI["age"] }' users.csv

# JSON Lines: keys of the first record become FI; nested values are raw JSON text
zawk -i jsonl '$FI["level"] == "error" { print $FI["ts"], json_value($FI["ctx"], "$.user.id") }' app.jsonl

# Parquet: $0 is the row as a JSON object, FI maps column names
zawk -i parquet '$FI["city"] == "Beijing" { print $0 }' people.parquet        # -> JSON Lines
zawk -i parquet -o csv '{ print $1, $2 }' people.parquet > people.csv

# JSON document: the records of an array, selected with a JSONPath after # (the root array by default)
zawk '{ print $FI["name"], $FI["age"] }' 'users.json#$.data'

# Markdown table input (first table of the file)
zawk -i md '{ print $FI["name"] }' README.md

# Excel: a sheet read as CSV from column A (-i xlsx is implied by .xlsx); #name selects the sheet
# (first sheet by default, name matched ignoring case); -H makes the first row the header.
# Dates are 2024-01-05 / 2024-01-05 13:45:10, times 08:30:00, booleans 1/0, empty cells "".
zawk -H '$FI["age"] > 40 { print $FI["name"] }' people.xlsx
zawk -i xlsx '{ print $1, $2 }' 'book.xlsx#Sheet2'
zawk inspect book.xlsx#Sheet2                  # sheet list, schema, sample rows and summary

# Output: the first printed record is the header row
zawk -i jsonl -o md 'NR==1 { print "name", "age" } $FI["age"] > 40 { print $FI["name"], $FI["age"] }' users.jsonl
# | name | age |
# | --- | --- |
# | Jackie | 48 |

# CSV -> Parquet; a column name may end with :TYPE (INT32, INT64, DOUBLE, BOOLEAN, DATE, TIMESTAMP, JSON, UUID, STRING,
# or AUTO to infer INT64/DOUBLE/BOOLEAN/DATE from the first 65536 rows)
zawk -i csv -H -o parquet --out-file users.parquet \
  'NR==1 { print "id:int64", "name", "age:int32" } { print $1, $2, $3 }' users.csv

# CSV -> JSON Lines with typed values: {"id":1,"name":"Jackie","age":48}
zawk -i csv -H -o jsonl --infer-types 'NR==1 { print "id", "name", "age" } { print $1, $2, $3 }' users.csv
# or per column: :number, :boolean, :auto, :string (default); a missing column is null
zawk -i csv -H -o jsonl 'NR==1 { print "id:number", "name", "vip:boolean" } { print $1, $2, $5 }' users.csv

# Aligned table with borders for a terminal, with per-column statistics
zawk -i csv -H -o table --summary 'NR==1 { print "name", "age" } { print $FI["name"], $FI["age"] }' users.csv

# Excel output (one sheet "Sheet1", bold frozen header row): numbers and true/false keep their type by default,
# 007 and integers of more than 15 digits stay text; a column name may end with :string, :number, :boolean or :auto
zawk -i csv -o xlsx --out-file users.xlsx '{ print }' users.csv
zawk -i parquet -o xlsx --out-file report.xlsx 'NR==1 { print "zip:string", "city", "total" } { print $1, $2, $3 }' sales.parquet
```

With `-o csv/tsv`, `print a, b` quotes and escapes fields correctly; never build CSV lines by concatenating `","`.
With `-o csv/tsv/jsonl/md/table/parquet/xlsx`, `print` (or `print $0`) of a CSV, Excel or database record writes its
fields as columns. In `-o md` and `-o table`, a newline inside a value is shown as a literal `\n` and a tab as `\t`;
`-o md` also escapes `|` as `\|` (`-o table` leaves it as is). `-o table` and `-o xlsx` write nothing until the
program ends. Excel limits a sheet to 1,048,576 rows and a cell to 32,767 characters (extra rows are dropped and long
text is cut, with a warning).

## String interpolation: f-strings

`f"..."` substitutes expressions, clearer than `sprintf` or concatenation when building SQL, JSON, URLs or
prompts:

```shell
zawk '{ print f"user {$1} logged in at line {NR}" }' login.txt
zawk -v table=users '{ print f"SELECT * FROM {table} WHERE id = {$1};" }' ids.txt
zawk '{ print f"{{\"id\": {$1}}}" }' ids.txt            # {{ and }} are literal braces
zawk '{ print f"{NR+1}: {toupper($2)} {$3 * 1.1:.2f} {$1:>10}" }' data.txt
```

- A placeholder is any expression: `{name}`, `{$1}`, `{NR+1}`, `{x > 0 ? \"pos\" : \"neg\"}`. Write `\"` for a quote
  inside a placeholder (the f-string ends at a bare `"`). An invalid expression is a syntax error; `{}` stays as text.
- A format spec follows a `:` (Python style): `[[fill]align][sign][#][0][width][.precision][type]`.
  `{p:.2f}` → `3.14`, `{n:5d}` → `   42`, `{n:<5}` → `42   `, `{n:05}` → `00042`, `{s:*^6}` → `**ab**`,
  `{n:x}` → `2a`, `{r:.1%}` → `25.6%`, `{s:.2}` → `he` (truncate). Without a type the value is formatted as a string;
  values are right-aligned by default. Thousands separators (`,`) and `=` alignment are not supported.
- A misspelled name (`{nmae}`) silently becomes an empty string.
- Escapes (`\t`, `\n`, `\"`) work as in normal strings. Values are not escaped: use `escape("sql", v)`,
  `escape("json", v)` first when the value comes from input.

## Default values: `??`

`l ?? r` is `r` when `l` is uninitialized, `""` or zero (also strings that look like zero: `"0"`, `"0.0"`), else `l`:

```shell
zawk '{ print "Hello " $2 ?? "Guest" }'   # binds tightest: "Hello " ($2 ?? "Guest")
zawk '{ n = cnt[$1] ?? -1 }'              # an array element is not created by ??
```

- `-u ?? 5` is `-(u ?? 5)`; `1 + u ?? 5` is `1 + (u ?? 5)`. Parenthesize a compound right side: `u ?? (a + 1)`.
- A field holding `0` counts as empty and is replaced: for numeric columns use `($3 == "" ? 0 : $3)`.

## Pattern matching: `match`

```shell
zawk '{ print match $2 { "info" => 1, "error" | "fatal" => 3, 3 => 3, /^W/ => 2, _ => 0 } }'
zawk '{ print match $NF { /(\d+)ms/ as m => m[1] > 1000 ? "slow" : "ok", _ => "-" } }'
```

- Patterns are string, number or regex literals, joined with `|`; the first matching arm wins.
- Numbers compare numerically with a field (strnum): `3 =>` matches `3.0`. `_` (last arm) is the default; with no
  match and no `_` the value is `""`. `/re/ as m` fills the array `m` (`m[0]` whole match, `m[1]` first group).
- Arms are separated by `,` or newlines. `match(s, re)` is still the builtin function.

## Syntax extensions

```shell
zawk '$1 !in seen { seen[$1]; print }'             # !in: same as !(k in arr); (i, j) !in arr works too
zawk '{ c[$1]++ } END { for (k, v in c) print k, v }'  # key-value loop: v = arr[k]
zawk '{ print $(-1), $(-2) }'                      # negative field index: $NF, $(NF-1)
zawk '{ assert(NF == 5, f"line {NR}: expect 5 fields, got {NF}") }'  # fail fast: exit 2, END is not run
zawk 'BEGIN { assert_eq(1 + 1, 2) }'               # on failure, prints left, right and the location
zawk test lib/ -k discount                         # run the test_* functions of the .awk files, without their rules
```

- `for (k, v in arr)` needs an array variable: `for (k, v in f())` is a syntax error; use `a = f(); for (k, v in a)`.
- Write `$(-1)` with parentheses: `$-1` is a syntax error. `$(-k)` with `k > NF` reads `""`. Only a literal `k` counts from the end; a computed negative index (`$(NF-2)` with `NF < 2`) is a fatal error.

## Recipes

```shell
# Group by + sum, as CSV (for-in order is unspecified: pipe to sort, or use asort)
zawk -i csv -H -o csv '{ s[$FI["dept"]] += $FI["salary"] } END { print "dept", "total"; for (k in s) print k, s[k] }' emp.csv

# Count + percentage of HTTP status codes in an access log
zawk '{ c[$9]++ } END { for (s in c) printf "%s %d %.1f%%\n", s, c[s], 100 * c[s] / NR }' access.log

# Deduplicate lines (exact), or with a bloom filter for huge inputs (constant memory)
zawk '!seen[$0]++' data.txt
zawk '!bf_icontains($0)' huge.txt

# Clean and normalize fields
zawk -i csv -o csv '{ print trim($1), snake_case($2), mask($3), tolower($4) }' raw.csv

# Dates: parse text to Unix time, format, bucket by day
zawk -i csv -H '{ t = mktime($FI["created_at"]); c[strftime("%Y-%m-%d", t, 1)]++ } END { for (d in c) print d, c[d] }' orders.csv

# Call an API per record and extract a value from the JSON response
zawk '{ r = http_get("https://api.example.com/users/" $1); print $1, r["status"], json_value(r["text"], "$.name") }' ids.txt

# Load CSV into SQLite (bind values as parameters; never concatenate them into SQL). Wrap the inserts in a
# transaction: each statement otherwise commits on its own, about 150 times slower
zawk -i csv -H 'BEGIN { sqlite_execute("app.db", "create table if not exists users(id int, name text)");
                        sqlite_execute("app.db", "begin") }
  { p[1] = $1; p[2] = $2; sqlite_execute("app.db", "insert into users values(?, ?)", p) }
  END { sqlite_execute("app.db", "commit") }' users.csv

# Query SQLite: each row is a CSV encoded string without column names (columns by position); decode it with
# from_csv, never split(..., ","), which breaks on values containing commas or quotes
zawk 'BEGIN { rows = sqlite_query("app.db", "select id, name from users"); for (i in rows) { f = from_csv(rows[i]); print f[2] } }'
# To read a whole table or query by column name, use it as the input instead
zawk '{ print $FI["name"] }' 'sqlite://app.db#select id, name from users where id > 10'

# Parse URLs / key-value pairs
zawk '{ u = url($1); print u["host"], pairs(u["query"], "&")["id"] }' urls.txt

# Hash, encode, IDs
zawk '{ print uuid(), digest("sha256", $0), encode("base64", $0) }' data.txt
```

### AI decisions with jev

`jev*` functions ask a typed yes/no/choice question about a record (text, or an array sent as a JSON object) using the
TypeSafe API; the `TYPESAFE_API_KEY` environment variable is required.

```shell
# filter: 1 when the probability of "yes" is above jev_threshold (default 0.5, or JEV_THRESHOLD env)
zawk 'jev($0, "the name is European")' names.txt
zawk -v jev_threshold=0.8 'jev($0, "the customer is angry")' tickets.txt

# rank: probability of yes, 0..1
zawk '{ printf "%.2f\t%s\n", jev_prob($0, "the customer is angry"), $0 }' tickets.txt | sort -rn

# classify: pick one option (comma separated text or array; for an array entry key => description)
zawk '{ print jev_choice($0, "which team should handle this?", "billing,technical,security,sales"), $0 }' tickets.txt

# score: probability-weighted position on ordered levels, 0 .. length(levels) - 1
zawk '{ print jev_score($0, "how luxurious is this product?", "budget,mid-range,premium,luxury"), $0 }' products.txt

# structured record: an array is sent as JSON
zawk -i csv -H '{ r["title"] = $FI["title"]; r["body"] = $FI["body"]; if (jev(r, "this is a bug report")) print $FI["id"] }' issues.csv
```

Failed requests are reported on stderr and return `0`, `-1` or an empty string.

## Standard library

Run `zawk functions <name>` for the signature and an example of any function below.

| Category | Functions |
|----------|-----------|
| Text | `length`, `strlen`, `char_at`, `chars`, `match`, `substr`, `sub`, `gsub`, `gensub`, `index`, `last_index`, `split`, `last_part`, `sprintf`, `hex`, `join_fields`, `join_csv`, `join_tsv`, `tolower`, `toupper`, `trim`, `truncate`, `capitalize`, `uncapitalize`, `camel_case`, `kebab_case`, `snake_case`, `title_case`, `pascal_case`, `isint`, `isnum`, `is`, `starts_with`, `ends_with`, `contains`, `eq_ignore_case`, `mask`, `pad`, `pad_start`, `pad_end`, `strcmp`, `similar`, `lines`, `words`, `repeat`, `wrap`, `default_if_empty`, `append_if_missing`, `prepend_if_missing`, `remove_if_end`, `remove_if_begin`, `quote`, `double_quote`, `parse`, `rparse`, `format_bytes`, `to_bytes`, `mkpass`, `figlet` |
| Escape | `escape` (json, csv, tsv, xml, html, sql, shell), `escape_csv`, `escape_tsv` |
| Parser | `url`, `data_url`, `shlex`, `path`, `semver`, `pairs`, `record`, `message`, `func`, `tuple`, `variant`, `parse_array`, `flags` |
| ID | `uuid` (v4/v7), `ulid`, `tsid`, `snowflake` |
| Array | `seq`, `uniq`, `asort`, `asorti`, `_max`, `_min`, `_sum`, `_mean`, `_median`, `_quantile`, `_var`, `_stddev`, `_mode`, `_count_distinct`, `_top`, `_join`, `bf_insert`, `bf_contains`, `bf_icontains` |
| Math | `rand`, `srand`, `int`, `float`, `strtonum`, `abs`, `ceil`, `floor`, `round`, `exp`, `log`, `log2`, `log10`, `sqrt`, `sin`, `cos`, `atan`, `atan2`, `intdiv0`, `min`, `max`, `eval`, `fend`, `mkbool` |
| Bitwise | `and`, `or`, `xor`, `compl`, `lshift`, `rshift`, `rshiftl` (negative values are a fatal error as in gawk, except the first argument of `rshiftl`; `compl(0)` is `2^53 - 1`; numbers and results keep their low 63 bits; shift counts of 64 and above shift everything out) |
| Date/Time | `systime`, `strftime`, `mktime`, `datetime`, `duration`, `date_add`, `date_diff`, `date_trunc`, `to_iso8601` |
| Color | `hex2rgb`, `rgb2hex` |
| Fake | `fake` |
| Decision | `jev`, `jev_prob`, `jev_choice`, `jev_score` |
| JSON | `from_json`, `json_flatten`, `to_json`, `json_value` (JSONPath), `json_query`, `json_has`, `json_get`, `json_set`, `json_del` |
| CSV | `from_csv`, `to_csv` |
| XML | `xml_value` (XPath), `xml_query` |
| HTML | `html_value` (CSS selector), `html_query` |
| Encoding | `encode`, `decode` (hex, base32, base58, base62, base64, base64url, url, ...) |
| Crypto | `digest`, `hash`, `hmac`, `encrypt`, `decrypt`, `jwt`, `dejwt` |
| KV | `kv_get`, `kv_put`, `kv_delete`, `kv_clear` (SQLite, `redis://`, `rediss://`, `nats://`) |
| Network | `http_get`, `http_post`, `send_mail` (REST API or SMTP), `s3_get`, `s3_put`, `publish` (NATS/Kafka/MQTT; `kafka://host/topic?key=value` sets the record key), `local_ip` |
| Database | `sqlite_query`, `sqlite_execute`, `pg_query`, `pg_execute`, `mysql_query`, `mysql_execute` |
| OS | `whoami`, `os`, `os_family`, `arch`, `pwd`, `user_home`, `getenv`, `system`, `system2` |
| I/O | `read_all`, `write_all`, `read_config`, `close`, `fflush` |
| Misc | `typeof`, `isarray`, `var_dump`, `pprint`, `log_debug`, `log_info`, `log_warn`, `log_error`, `version`, `exit`, `assert` |

Functions returning `map[...]` return an array: index the call directly (`url($1)["host"]`) or assign it
(`u = url($1)`). Use `var_dump(x)` to see an array's structure on stderr.

Text is UTF-8: `length`, `substr`, `index` and `match` count characters (`length("你好Hello")` is 7), `strlen` counts
bytes. Input that is not valid UTF-8 (GBK, Latin-1, binary) is kept byte for byte: hashes and encodings
(`digest`, `hmac`, `encode`) work on the raw bytes, and text functions never replace invalid bytes with U+FFFD.

Functions using external services (network, KV, databases, crypto keys) don't stop the program on failure: they print
one warning to stderr and return an empty value (`0` for an HTTP status). The failure sets `ERRNO` (e.g.
`sqlite_query: no such table: users`), which is never cleared by a success: `ERRNO = ""; r = f(...); if (ERRNO != "") ...`.
Shorter: `r = try f(...) else fallback(ERRNO)` clears `ERRNO`, evaluates `f(...)` without printing warnings, and
gives `fallback(ERRNO)` on failure (`else` is required; `try` is a keyword).
`http_get`/`http_post` only fail (status `0`, `ERRNO` set) on a network error: a 4xx/5xx response is returned as
is without `ERRNO`, so check the status (`r = http_get(url); body = r["status"] ~ /^2/ ? r["text"] : ""`) or use
`body = try read_all(url) else ""`, which fails on an HTTP status >= 400.

## Pitfalls (differences from gawk)

- `-i` is the input format, not gawk's `-i library`.
- `ARGV`/`ARGC` are read-only: assigning `ARGV[1]` doesn't change the input files.
- No arrays of arrays (`a[i][j]`): use `a[i, j]` with `SUBSEP`, or `(i, j) in a`.
- `IGNORECASE = 1` works as in gawk: regex matching, `FS`/`RS`, string comparisons and `index()` ignore case (a
  single-character string separator such as `FS = "x"` stays literal); `(?i)` in a regex also works. No
  `FIELDWIDTHS`/`FPAT`: use `-i csv`, `substr()` or `match()`. No `RT`.
- gawk's `switch` is supported (`case` labels are numeric/string/regex constants, bodies fall through until
  `break`). Not supported: `BEGINFILE`/`ENDFILE`, `@include`, co-processes `|&`.
  Sort with `asort(arr)` (values), `asorti(arr, keys)` (indices) or pipe the output to `sort`.
- `f"..."` is always an f-string, never the variable `f` concatenated with a string: write `f "-"` (with a space)
  or `(f) "-"` to concatenate a variable named `f`. Likewise `r"..."` and `r#"..."#` are raw strings: with `r = 5`,
  `print r"x"` prints `x` (gawk: `5x`); write `r "x"` or rename the variable.
- Missing elements of numeric arrays print `0`, not an empty string, and referencing them creates them: test with
  `(k in a)`.
- `$FI["name"]` for a column that doesn't exist is `""`, without any warning: check the header with
  `zawk inspect f` and run `zawk lint ... f` with the input file, which reports unknown columns (W126).
- Regex matches are leftmost-longest, and `?` after a quantifier is not lazy: `a+?` is `(a+)?`, so write
  `/<[^>]+>/` rather than `/<.+?>/` for a short match (gawk stops at the shortest there).
- `getline line < dir "/" name` reads the concatenated path; parenthesize to be portable.
- `next`/`nextfile` are not allowed inside functions.
- A user-defined function with the name of a stdlib function (`trim`, `max`, ...) takes precedence over it;
  POSIX/gawk builtins (`length`, `substr`, `split`, ...) cannot be redefined.

## Scripts as tools

A script with metadata comments gets a generated `--help`, so it can be reused as a documented tool:

```awk
#!/usr/bin/env -S zawk -E
# @desc Summarize orders by day
# @meta version 0.1.0
# @var tz time zone offset in hours
# @env DB_URL database url

{ c[strftime("%Y-%m-%d", mktime($1, 0, tz + 0))]++ }
END { for (d in c) print d, c[d] }
```

```shell
zawk -f orders.awk --help            # description, variables and environment from the metadata
zawk -v tz=8 -f orders.awk orders.txt
zawk init report                     # create report.awk from a template
```

## Tips for agents

- Check the schema and first records before writing a program: `zawk inspect -n 5 f` (any format), or
  `zawk -i jsonl 'NR<=3; NR==3 { exit }' f.jsonl`. `NR<=3` alone still reads the whole input (a full table scan for
  a database): add `NR==3 { exit }`.
- Lint with the input file before a long run: `zawk lint -i csv -H -f prog.awk f.csv`.
- Use `-o md` for results shown to the user, `-o csv`/`-o jsonl` for results consumed by another tool, `-o xlsx`
  for a spreadsheet a person will open.
- `--stats` reports how many records were read and printed (on stderr, as JSON): a quick check that a filter
  matched what you expected.
- Quote the program with single quotes; inside it, write a single quote as `'\''` in the shell, or move the program
  to a file with `-f`.
- Errors are printed on stderr: `zawk: syntax error at line L, column C: ...` (exit code 1, like usage errors) or
  `zawk: fatal: ...` for type and runtime errors (exit code 2).
- References: `info/stdlib.md` (full stdlib docs), `info/reference.md` (awk builtins), `info/faq.md`.
