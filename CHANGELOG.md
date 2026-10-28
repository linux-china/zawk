# Unreleased

* Add `try E else F`: evaluate `E` without printing stdlib warnings, and give `F` (which can read `ERRNO`) if a stdlib function, `getline` or `close` failed in `E`, e.g. `body = try read_all(url) else ""` (`read_all` fails on an HTTP status >= 400; `http_get`/`http_post` only fail on a network error, so check `r["status"]` for HTTP errors). A failure an inner `try` handled (also in a called function) does not count for an outer one; fatal errors are not caught. **Behavior change**: `try` is a keyword and can no longer be used as a variable or function name (`try = 1`, `try++`, `try[1]`, `print try`, `function f(try)`, `function try()`, `-v try=1` are now errors, reported as "`try` is a keyword"); rename such variables, parameters or functions
* Add Excel input `-i xlsx` (with the `calamine` crate), the default when every input file ends with `.xlsx` (or `.xls`, `.xlsm`, `.xlsb`, `.ods`): a sheet is read as CSV from column A, `book.xlsx#Sheet2` selects the sheet (the first one by default; the name is matched ignoring case), and with `-H` the first row is the header, e.g. `zawk -H '{ print $FI["name"] }' tests/demo.xlsx`. Dates are `2024-01-05` or `2024-01-05 13:45:10`, times `08:30:00`, booleans `1`/`0`. Works with `--summary`, `zawk inspect` (the first row is the header), `zawk lint` and `zawk llm`; `-o xlsx` is not supported
* Add `similar(a, b)`: the Jaro-Winkler similarity of two texts, from `0` to `1` (a common prefix counts more), for fuzzy matching of names and typos, e.g. `similar($1, "apple") > 0.8 { print }`
* Add database tables and queries as input: `zawk '{ print $FI["name"] }' 'mysql://root:123456@127.0.0.1:3306/test#people'`. The URL is `sqlite://PATH`, `mysql://...` or `postgresql://...` (`postgres://`), followed by `#` and a table name, or a SQL query when the text contains whitespace (`'sqlite://./demo.db#select nick, age from user where age > 30'`). Rows are streamed and read like `-i csv -H`: `FI` maps the column names, NULL and BLOB (BYTEA, binary) values are empty. Works with `-o table --summary`, `zawk lint` and `zawk llm`; `zawk inspect URL` shows the schema with the database column types, sample rows and the summary. Passwords are masked in messages

* **Behavior change**: `json_value`, `json_query`, `json_has`, `json_get`, `json_set` and `json_del` report text that is not valid JSON (an HTML error page, a truncated document) with a warning and set `ERRNO`, so that `try json_value(r["text"], "$.id") else ...` falls back and an invalid document is told apart from a missing value; the results are unchanged (`""`, an empty array, `0`, or the text itself for `json_set`/`json_del`). `json_set`/`json_del` also warn on valid JSON that is not an object (`[1,2]`). Empty text is still no document, without a warning (`json_set("", k, v)` creates an object)
* **Behavior change**: `publish(url, body)` accepts only `nats://`, `nats+tls://`, `mqtt://`, `mqtts://` and `desktop://title` (the scheme is matched ignoring case, e.g. `NATS://`); any other URL (`redis://`, `nats//typo`, a plain name) is reported with a warning and sets `ERRNO` instead of silently showing a desktop notification. Use `desktop://Job finished` for a notification; passwords in the title are masked
* **Behavior change**: `-i jsonl`/`-i json`: blank lines and lines that are not valid JSON are skipped instead of read as records (`NF=0` or empty fields) that counted in `NR`; an invalid line is still reported with a warning and sets `ERRNO`, and the schema comes from the first valid record

# Version 0.6.5 (2026-10-26)

* Add `zawk llm [FILE...] QUESTION`: print a prompt asking an LLM to write a zawk program that answers the question (the last argument, easy to edit from the shell history) about the input files, e.g. `zawk llm -i csv -H family.csv 'how many adults?' | llm -x`. The prompt has the command line of the run with the meaning of `-i`, `-F`, `-H` and `-o`, the first 10 lines of every file (`-n`; the first rows for Parquet), the schema and summary of `zawk inspect` for CSV, TSV, JSON, JSON Lines and Parquet input, the zawk language notes and the builtin function reference, and the `zawk lint` command to check the program; a short program is answered with its complete command line too
* Add NATS subjects as input: `zawk 'MSG_SUBJECT ~ /demo/ { print }' 'nats://localhost:4222/logs.>'` reads the messages of the subject (wildcards `*` and `>` allowed) until Ctrl+C, then runs `END` and exits. Each message is read as an input file of its own in the input format (`-i json`, `-i jsonl`, `-i csv -H`, ...; text split by `FS` by default), with `MSG_SUBJECT`, `MSG_HEADERS` (e.g. `MSG_HEADERS["content-type"]`) and `MSG_BODY`; its output is written at once. A payload that cannot be parsed sets `ERRNO`
* Add the `match` expression: `match subject { pat => value, ... }` returns the value of the first arm whose pattern matches, e.g. `level = match $2 { "info" => 1, "error" | "fatal" => 3, /^W/ => 2, _ => 0 }`. A pattern is a string, a number or a regex literal, or several joined with `|`; numbers compare as numbers with a strnum subject, as in gawk's `switch` (so `3 => ...` matches `3.0`). `/re/ as m => value` binds the groups of the match to the array `m`, visible only in `value` (`/(\d+)ms/ as m => m[1] > 1000 ? "slow" : "ok"`). `_` matches anything and must be the last arm; without it, the value is `""` when nothing matches. The subject is evaluated once; arms are separated by commas or newlines. `match(s, re)` is still the builtin; `match (x) {` starts a match expression

# Version 0.6.4 (2026-10-25)

* Add `@include` support
* **Behavior change**: remove libSQL support: `libsql_query` and `libsql_execute` and the `libsql` crate are removed; use `sqlite_query`/`sqlite_execute` for local databases
* Reduce final binary size
* Fix some bugs

# Version 0.6.3 (2026-10-21)

* Add raw string literals, like Rust's: `r"C:\Program Files\Rust"` is taken verbatim, backslashes are not escapes (no more `"\\d+"` in dynamic regexes: `$0 ~ r"\d+"`). To include `"`, put the same number of `#`s around the quotes: `r#"say "hi""#`, `r##"a"#b"##`. A raw string can span lines. **Behavior change**: `r"..."` (no space) is now a raw string, not the variable `r` concatenated with a string; write `r "..."` for the latter
* Add the `??=` operator: `l ??= r` is `l ?? (l = r)`, so `r` is evaluated and assigned only when `l` is uninitialized, empty or zero (as for `??`); the subscript of an array element is evaluated once. E.g. `{ OFS ??= ","; total[$1] ??= 0 }`
* Add the `!in` operator, like Kotlin's `!in` (Python's `not in`): `k !in arr` is `!(k in arr)`, e.g. `$1 !in seen { seen[$1]; print }`; multi-dimensional keys work too: `(i, j) !in arr`
* Add key-value iteration `for (k, v in arr) body`, like Python's `for k, v in d.items()`: the same as `for (k in arr) { v = arr[k]; body }`, e.g. `END { for (k, v in cnt) print k, v }`; on an array made by `split` it works like `enumerate`. `arr` must be an array variable: `for (k, v in f())` is a syntax error (assign the result first: `a = f(); for (k, v in a)`)
* Add negative field indexes: `$(-k)` is `$(NF + 1 - k)`, so `$(-1)` is `$NF` and `$(-2)` is `$(NF-1)`. Reading before the first field gives `""`, assigning there is a fatal error; the parentheses are required (`$-1` is a syntax error). Only a literal index counts from the end: a negative index computed at runtime (e.g. `$(NF-2)` on a one-field record) is a fatal error, as in gawk
* Add `assert(cond, msg)`: when `cond` is false, print `msg` to stderr and exit at once with code `2`, without running `END` (unlike `exit 2`), e.g. `{ assert(NF == 5, f"line {NR}: expect 5 fields, got {NF}") }`
* Add `local` variables: `{ local a = $1; ... }` declares `a` for the rest of the rule (or function body), including nested blocks; it shadows a global `a`, which other rules still see. A local is uninitialized again on each run of the rule (`{ local n; n++; print n }` prints `1` for every record), `local arr[]` is an empty array, and `local x = x + 1` reads the outer `x`. Several can be declared at once (`local i, s = "", seen[]`). In a function, locals are new on each call, like the extra-parameter idiom. Special variables (`NR`, `FS`, `SUBSEP`, ...) and function names cannot be local, a name cannot be declared twice in one `local` statement, and a scalar local used as an array is an error that suggests `local x[]`. **Behavior change**: `local` is a keyword and can no longer be used as a variable or function name (`local = 1` and `function local()` are now syntax errors); rename such variables or functions
* A failing stdlib function (database, HTTP, KV, S3, crypto, ...) now also sets `ERRNO` to its message (e.g. `sqlite_query: no such table: users`), and a network error of `http_get`/`http_post` (status `0`) sets `ERRNO` to the reason, so `ERRNO = ""; r = sqlite_query(db, sql); if (ERRNO != "") ...` tells a failure from an empty result
* Database functions take optional positional parameters: `sqlite_query`/`sqlite_execute`, `libsql_*`, `mysql_*` and `pg_*` accept an array as a third argument, bound in key order to a prepared statement (`p[1] = $1; p[2] = $2; sqlite_execute(db, "insert into t values(?, ?)", p)`), so values containing `'` work and input data cannot inject SQL. Placeholders are `?` (SQLite, libSQL, MySQL) or `$1`, `$2`... (PostgreSQL); PostgreSQL converts the text values to the inferred parameter types (bool, integers, floats, numeric, json, uuid, inet, bytea, date/time), with an empty value as NULL for non-text types. PostgreSQL error messages now include the cause (`db error: ERROR: relation "t" does not exist`) instead of only `db error`
* `-i jsonl`/`-i json`: a key missing from the first record is no longer dropped. It gets the next column when a later record has it (NF grows from that record on) and is added to `FI`, so `$FI[k]` with a key only known at run time finds it; a scalar first record no longer leaves the later objects with `NF=0`
* **Behavior change**: `s3_get`/`s3_put` failures are a warning and an empty result, like other stdlib functions, instead of a fatal panic

# Version 0.6.2 (2026-10-18)

* Add thousands separators: f-string format specs take Python's `,` and `_` groupings (`f"{n:,}"` is `1,234,567`, `{p:,.2f}`, `{n:_d}`, `{n:012,}` is `0,001,234,567`), and `printf`/`sprintf` take the `'` flag (`,`, as gawk in an English locale, independently of the locale) and the `_` flag (`_`): `printf "%'d", 1234567`. Grouping applies to `d i u e E f F g G` and to `%s` values that look like numbers; with `0` the padding zeros are grouped too, as in Python
* Add the `??` operator: `name ?? "Guest"` is `"Guest"` when `name` is uninitialized, empty, zero or a string that looks like zero (`"0"`, `"0.0"`, `"+0"`, as with strnums), and `name` otherwise; the left side is evaluated once. It binds tighter than any other operator, so `"Hello " name ?? "Guest"` is `"Hello " (name ?? "Guest")`
* Add `zawk lint`: check an AWK program without running it and report the problems as JSON (`valid`, `errors`, `warnings` and `diagnostics`, each with a code, severity, message, file, line, column, the source line and a caret), so that editors and AI agents can check a program before running it. Besides syntax and type errors, it reports gawk features that zawk accepts but ignores (e.g. `FIELDWIDTHS`), variables read but never set, `printf` formats with too few arguments, calls with more arguments than the function has parameters, unknown functions, assignments to read-only variables, and `NR`, `$0`, `$1` used in `BEGIN`. The program comes from the operand, `-f` files, `-e` sources or stdin; `-v`, `-H`, `-i`, `-A` and `-p` describe how the program will run, e.g. `-pr` also reports code whose meaning changes in parallel. Exit code is 1 when there are errors (warnings do not count)
* Add `--lint` option: lint the program before running it; the JSON report goes to stderr when there are diagnostics, and the program does not run when there are errors
* Add `zawk inspect FILE`: the schema, the first 10 rows and a DuckDB `SUMMARIZE`-like summary (min, max, approx_unique, avg, std, q25, q50, q75, count, null_percentage) of a CSV, TSV, JSON Lines or Parquet file, as monospaced tables (default) or Markdown (`-o markdown`), so that people and AI agents can look at the data before writing a program
* Add `zawk fmt`: format AWK programs (files, `-e` text or stdin): blocks are indented (`--indent N`, default 4, or `--tabs`), one statement per line, normalized spaces between tokens (`if (`, `a = b + 1`, `f(x, y)`, `$NF`), `} else` on one line; comments, blank lines (at most one in a row), literals and line breaks inside expressions are kept. `-w` writes the files back, `--check` lists the files that are not formatted and exits with 1. The result is checked to parse to the same program as the original
* f-string placeholders now take any expression and an optional Python-style format spec: `f"{NR+1}: {price:.2f} {name:>10}"`. A spec is `[[fill]align][sign][#][0][width][.precision][type]` with `align` one of `<`, `>`, `^` and `type` a printf conversion (`d i o x X e E f F g G s c`) or `%`. **Behavior change**: `{x+1}` used to stay as literal text and is now evaluated; an invalid placeholder expression is a syntax error
* Add gawk's `ERRNO`: the system error message of the last failed `getline < file` (e.g. `No such file or directory`) or `close()` of a redirection that was never opened, so `if ((getline line < f) < 0) print "cannot read " f ": " ERRNO` works as in gawk

# Version 0.6.1 (2026-10-12)

* Add [Jev support](https://github.com/linux-china/zawk/blob/master/info/stdlib.md#decisionjev) 
* **Behavior change**: CSV now follows RFC 4180. `-i csv` no longer interprets `\n`, `\t` and `\\` inside quoted fields (`"C:\new"` stays `C:\new`), and keeps a lone `\r` inside a field (only the `\r` of a CRLF line ending is dropped). `-o csv` and `escape_csv()`/`escape("csv", s)` keep line breaks inside quotes instead of writing `\n`/`\t`, also quote fields with `\r` or leading/trailing spaces, and `-o csv` ends records with `\n` instead of `\r\n` (use `-v 'ORS=\r\n'` for CRLF)
* Add `PROCINFO["sorted_in"]` support for `for (k in arr)`: all of gawk's predefined orders (`@ind_str_asc`, `@ind_num_desc`, `@val_num_asc`, `@val_type_desc`, ...). A user-defined comparison function is a fatal error instead of being silently ignored
* `IGNORECASE` now also applies, as in gawk, to string comparisons (`==`, `!=`, `<`, ..., and `switch` string cases) and `index()`, comparing Unicode characters case-insensitively, and to single-character regex literals such as `split(s, a, /x/)`. A single-character string separator (`FS = "x"`, `split(s, a, "x")`) stays literal
* Add gawk's `switch` statement: `switch (expr) { case 1: ... case "str": ... case /regex/: ... default: ... }`. Numeric and string labels are compared with `==`, regex labels with `~`; a body falls through to the next case unless it ends with `break`, and `continue` applies to the enclosing loop. Duplicate case values and a duplicate `default` are syntax errors, as in gawk
* Add f-string interpolation: `f"Hello, {name}"`, `f"{$1} at line {NR}"`. Only variable names and field references (`{$1}`, `{$NF}`) are substituted; `{{`/`}}` are literal braces
* Add JSON Lines input format `-i jsonl` (alias `ndjson`): keys of the first record populate `FI`, e.g. `zawk -i jsonl '{ print $FI["name"] }' demo.jsonl`
* Add parquet input format `-i parquet`: keys of the first record populate `FI`, e.g. `zawk -i parquet '{ print $FI["name"] }' demo.parquet`

# Version 0.6.0 (2026-10-10)

* Fix lots of bugs reported by LLM
* Remove LLVM backend and keep Cranelift only
* Update to Rust edition 2024

# Version 0.5.25 (2024-12-29)

* Add `getenv("NAME", "default value")` function to get environment variable
* Add `strlen(text)` function to get text bytes length:  `strlen("你好")` to `6`
* Fix `substr(s,index,len)`: 0 and negative index support now and same with gawk.

# Version 0.5.24 (2024-12-22)

* Fix `_join` with wrong sequence

# Version 0.5.23 (2024-12-10)

* Add `chars($1)` function to return char array of text
* Use fs if sep empty in `split(text,arr,sep)`

# Version 0.5.22 (2024-10-18)

* Add `eval(formula, context)` function for math calculation
* Add [Resend](https://resend.com/emails) mail service support for `send_mail()`, and environment variable is `RESEND_API_KEY`.
* Documentation to make associative array quickly: `array[$1] = $2`, `arr = record("{host:localhost,port:1234}")`, `arr = pairs("a=b,c=d")`.

# Version 0.5.20 (2024-10-05)

* Add `smtp_send(url, from, to, subject, body)` function to send email
* Add MQTT support: `publish("mqtt://servername:1883/topic", body)`
* Add `system2(cmd)`: different from `system(cmd)`, and it will return an array with `code`, `stdout`, `stderr`.
* Add `cargo-binstall` and `cargo-dist` support
