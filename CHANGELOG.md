# Unreleased

* Add `zawk inspect FILE`: the schema, the first 10 rows and a DuckDB `SUMMARIZE`-like summary (min, max, approx_unique, avg, std, q25, q50, q75, count, null_percentage) of a CSV, TSV, JSON Lines or Parquet file, as monospaced tables (default) or Markdown (`-o markdown`), so that people and AI agents can look at the data before writing a program
* f-string placeholders now take any expression and an optional Python-style format spec: `f"{NR+1}: {price:.2f} {name:>10}"`. A spec is `[[fill]align][sign][#][0][width][.precision][type]` with `align` one of `<`, `>`, `^` and `type` a printf conversion (`d i o x X e E f F g G s c`) or `%`. **Behavior change**: `{x+1}` used to stay as literal text and is now evaluated; an invalid placeholder expression is a syntax error

# Version 0.6.1 (2026-09-12)

* Add [Jev support](https://github.com/linux-china/zawk/blob/master/info/stdlib.md#decisionjev) 
* **Behavior change**: CSV now follows RFC 4180. `-i csv` no longer interprets `\n`, `\t` and `\\` inside quoted fields (`"C:\new"` stays `C:\new`), and keeps a lone `\r` inside a field (only the `\r` of a CRLF line ending is dropped). `-o csv` and `escape_csv()`/`escape("csv", s)` keep line breaks inside quotes instead of writing `\n`/`\t`, also quote fields with `\r` or leading/trailing spaces, and `-o csv` ends records with `\n` instead of `\r\n` (use `-v 'ORS=\r\n'` for CRLF)
* Add `PROCINFO["sorted_in"]` support for `for (k in arr)`: all of gawk's predefined orders (`@ind_str_asc`, `@ind_num_desc`, `@val_num_asc`, `@val_type_desc`, ...). A user-defined comparison function is a fatal error instead of being silently ignored
* `IGNORECASE` now also applies, as in gawk, to string comparisons (`==`, `!=`, `<`, ..., and `switch` string cases) and `index()`, comparing Unicode characters case-insensitively, and to single-character regex literals such as `split(s, a, /x/)`. A single-character string separator (`FS = "x"`, `split(s, a, "x")`) stays literal
* Add gawk's `switch` statement: `switch (expr) { case 1: ... case "str": ... case /regex/: ... default: ... }`. Numeric and string labels are compared with `==`, regex labels with `~`; a body falls through to the next case unless it ends with `break`, and `continue` applies to the enclosing loop. Duplicate case values and a duplicate `default` are syntax errors, as in gawk
* Add f-string interpolation: `f"Hello, {name}"`, `f"{$1} at line {NR}"`. Only variable names and field references (`{$1}`, `{$NF}`) are substituted; `{{`/`}}` are literal braces
* Add JSON Lines input format `-i jsonl` (alias `ndjson`): keys of the first record populate `FI`, e.g. `zawk -i jsonl '{ print $FI["name"] }' demo.jsonl`
* Add parquet input format `-i parquet`: keys of the first record populate `FI`, e.g. `zawk -i parquet '{ print $FI["name"] }' demo.parquet`

# Version 0.6.0 (2026-09-10)

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
