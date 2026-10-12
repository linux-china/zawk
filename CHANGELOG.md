# Unreleased

* **Behavior change**: CSV now follows RFC 4180. `-i csv` no longer interprets `\n`, `\t` and `\\` inside quoted fields (`"C:\new"` stays `C:\new`), and keeps a lone `\r` inside a field (only the `\r` of a CRLF line ending is dropped). `-o csv` and `escape_csv()`/`escape("csv", s)` keep line breaks inside quotes instead of writing `\n`/`\t`, also quote fields with `\r` or leading/trailing spaces, and `-o csv` ends records with `\n` instead of `\r\n` (use `-v 'ORS=\r\n'` for CRLF)
* Fix CSV quotes in the middle of a field: like gawk, only a `"` at the start of a field opens a quoted field, and a `"` inside a quoted field that is not followed by `"`, `,` or a line end is literal. `ab"c,d` is now `ab"c` and `d` instead of one field `abc,d`. Well-formed CSV still uses the SIMD fast path
* Fix `zawk ... | head` printing `Broken pipe (os error 32)` and exiting with 2: a closed stdout now ends the program quietly with exit code 141 (128 + SIGPIPE), as the shell reports for gawk
* Fix stdlib functions printing non-UTF-8 text (GBK, Latin-1, binary) as a byte array like `[97, 98, 255]`: `trim()` and `truncate()` now keep invalid bytes as is, and other functions replace them with U+FFFD

* Add f-string interpolation: `f"Hello, {name}"`, `f"{$1} at line {NR}"`. Only variable names and field references (`{$1}`, `{$NF}`) are substituted; `{{`/`}}` are literal braces
* Add JSON Lines input format `-i jsonl` (alias `ndjson`): keys of the first record populate `FI`, e.g. `zawk -i jsonl '{ print $FI["name"] }' demo.jsonl`

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
