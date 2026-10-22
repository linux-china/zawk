zawk Standard Library
====================

Standard library for AWK with text, math, crypto, kv, database, network etc.

zawk stdlib Cheat Sheet: https://cheatography.com/linux-china/cheat-sheets/zawk/

Functions working with external services or user supplied keys (network, kv, crypto, ...) do not stop
the program on failure: they print a warning to stderr, such as
`zawk: warning: kv_get: failed to connect to redis://127.0.0.1:6379: Connection refused`, and return
an empty value (an empty string or array, or `0` for an HTTP status). Each distinct warning is printed
once (at most 1024 of them), and the number of warnings not shown is printed when the program exits.

The failure also sets `ERRNO` to the message without the `zawk: warning: ` prefix (for `http_get`/`http_post`,
a network error that makes the status `0`), so a script can tell a failure from an empty result.
`ERRNO` is not cleared when a call succeeds, so clear it first:

```awk
ERRNO = ""
rows = sqlite_query("app.db", "select name from users")
if (ERRNO != "") { print "query failed: " ERRNO > "/dev/stderr"; exit 1 }
```

A user-defined function with the same name as a stdlib function (such as `trim`, `max` or `abs`)
takes precedence over it in the whole program, so existing AWK programs that define these helpers
keep working. The builtin functions of POSIX awk and gawk (`length`, `substr`, `split`, `gsub`,
`sprintf`, ...) cannot be redefined, as in gawk.

# Text functions

Text is encoding with utf-8 by default.

### length

Get unicode character length of text: `length($1)`, `length("你好")` to `2`.

### strlen

Get byte length of text: `strlen($1)`, `strlen("你好")` to `6`.

### char_at

Get char at index: `char_at($1, 1)`, starts from 1. If index is out of range, return empty string.

### chars

Return char array of text: `ar=chars($1)`, starts from 1.

### match(text, re)

if string text matches the regular expression in re. If s matches, the RSTART variable is set with the start of the
leftmost match of re, and RLENGTH is set with the length of this match.

### substr(text, i[, j])

The 1-indexed substring of string s starting from index i and continuing for the next j characters or until the end of s
if i+j exceeds the length of s or if s is not provided.
As in gawk, a start before the first character starts at the first character and keeps the length:
`substr("hello", 0, 3)` and `substr("hello", -1, 3)` are both `hel`. Positions count characters, not bytes.

### sub(re, text, s)

Substitutes t for the first matching occurrence of regular expression re in the string s.

### gsub(re, text, s)

Like sub, but with all occurrences substituted, not just the first.

### index(haystack, needle)/last_index()

* `index(text, 's')`: the first index within haystack in which the string needle occurs, 0 if needle does not appear.
* `last_index(text,'s')`: the last index within haystack in which the string needle occurs, 0 if needle does not appear.

Positions count characters, as `length()` and `substr()` do: `index("中文abc", "a")` is `3`, so
`substr(s, index(s, x))` works on multibyte text. `match()`, `RSTART` and `RLENGTH` count characters too.

### split(text, arr[, fs[, seps]])

Splits the string s according to fs, placing the results in the array `arr`. If fs is not specified then the FS variable
is
used to split s.

As in gawk, the optional array `seps` receives the separators: `seps[i]` is the separator between `arr[i]` and
`arr[i+1]`. When fs is `" "`, leading and trailing blanks are kept in `seps[0]` and `seps[n]`, so the original text can be
rebuilt from the parts: `n = split("a=1; b=2", parts, /[=;]/, seps)`.

### patsplit(text, arr, re[, seps])

The counterpart of `split()` (and of gawk's `FPAT`, which zawk does not support): the matches of the regex `re` in the
string become the elements of `arr`, and the text around them goes to the optional array `seps`, as in gawk:
`seps[0]` is the text before `arr[1]` and `seps[i]` the text after `arr[i]` (both kept even when empty). An empty
match is a field unless it directly follows another match. Returns the number of matches.

```awk
n = patsplit("a1b22c333", nums, /[0-9]+/)               # 3: nums[1]="1", nums[2]="22", nums[3]="333"
n = patsplit("x,\"y,z\",,w", f, /([^,]*)|("[^"]+")/)    # CSV-like fields: x, "y,z", "", w
```

### last_part(s [, sep])

Get last part with sep: `last_part("a/b/c", "/")` to `c`.

If sep is not provided, zawk will use `/` to search first, if not found, zawk will use `.` to search.

* `last_part("a/b/c")` to `c`
* `last_part("a.b.c")` to `c`

### sprintf(fmt, s, ...)

Returns a string formatted according to fmt and provided arguments. The goal is to provide the semantics of the libc
sprintf function.

### printf(fmt, s, ...) [>[>] out]

Like sprintf but the result of the operation is written to standard output, or to out according to the append or
overwrite semantics specified by > or >>. Like print, printf can be called without parentheses around its arguments,
though arguments are parsed differently in this mode to avoid ambiguities.

### gensub(re, repl, how[, target])

As in gawk: returns target (`$0` by default) with the `how`-th match of re replaced by repl, or all matches if `how`
starts with `g`. Target is not changed, and `\1`..`\9` in repl refer to the groups of re:
`gensub(/(a)(b)/, "\\2\\1", "g", "abab")` is `baba`.

### hex(s)

Returns the hexadecimal integer (e.g. 0x123abc or ff, after optional blanks and sign) encoded in s, or 0 otherwise.
The digits are read as a 64-bit unsigned value (wrapping around beyond 16 digits) with the same bits as the result,
so a 64-bit hash or id keeps all its bits: `hex("ffffffffffffffff")` returns -1.

`hex("0xFF")` returns 255. Please use `strtonum("0x11")` instead.

### join_fields(i, j[, sep])

Returns columns i through j (1-indexed, inclusive) concatenated together, joined by sep, or by OFS if sep is not
provided.

### join_csv(i, j)

Like join_fields but with columns joined by , and escaped using escape_csv.

### join_tsv(i, j)

Like join_fields but with columns joined by tabs and escaped using escape_tsv.

### tolower(s)

Returns a copy of s where all uppercase ASCII characters are replaced with their lowercase counterparts; other
characters are unchanged.

### toupper(s)

Returns a copy of s where all lowercase ASCII characters are replaced with their uppercase counterparts; other
characters are unchanged.

### strtonum:

The numeric value of a string, as in gawk: `strtonum("0x11")` is `17` (hexadecimal) and `strtonum("017")` is `15`
(octal, a leading `0`). Digits `8` or `9`, a `.` or an exponent make the number decimal: `strtonum("018")` is `18`.
Input that looks like a decimal number, such as the field `017`, is decimal: `strtonum($1)` is `17`, while
`strtonum($1 "")` is `15`. Use `float()` for `0o` and `0b` prefixes.

### trim

Trim text with space by default. `trim($1)`.
Trim text with chars with `trim($1, "[]()")`

### truncate

`truncate($1, 10)` or `truncate($1, 10, "...")`

### capitalize/uncapitalize:

`capitalize("hello") # Hello` or `uncapitalize("Hello") # hello`

### camel_case: functions/attribute names

`camel_case("hello World") # helloWorld`

### kebab_case: file names

`kebab_case("hello world") # hello-world`

### snake_case: functions/attribute names

`snake_case("hello world") # hello_world`

### pascal_case/title_case: Component

`title_case("hello world") # Hello World`

### isint

`isint("123")`

### isnum

`isnum("1234.01")`

### is(format,txt)

Validate text format, such as: `is("email", "demo@example.com")`. Format list:

- email
- url
- phone
- ip: IP v4/v6

### starts_with/ends_with/contains

The return value is `1` or `0`.

- `starts_with($1, "https://")`
- `ends_with($1, ".com")`
- `contains($1, "//")`

Why not use regex? Because starts_with/ends_with/contains are easy to use and understand.
Most libraries include these functions, and I don't want AWK stdlib weird.

**Tips**: You can use regex expression for `is_xxx()`、`contains()`、`starts_with()`、`ends_with()` functions.

- is_int: `/^\d+$/`
- contains: `/xxxx/`
- starts_with: `/^xxxx/`
- ends_with: `/xxxx$/`

### mask

`mask("abc@example.com")`, `mask("186612347")`

### pad

- pad:  `pad($1, 10, "*")` to `***hello**`, `pad_start($1, 10, "*")` to `***hello`, `pad_end($1, 10, "**")`
  to `hello***`,

### strcmp:

text compare `strcmp($1, $2)` return -1, 0, 1

### lines

Split text to none-empty lines: `lines(text)`: array of text.

### words

text to words: `words("hello world? 你好") # ["hello", "world", "你", "好"]`

### repeat

`repeat("*",3) # ***`

### default_if_empty

Return default value if text is empty or not exist.

`default_if_empty("   ", "demo") # demo` or `default_if_empty(var_is_null, "demo") # demo`

### append_if_missing/prepend_if_missing

Add suffix/prefix if missing/present

- `append_if_missing("nats://example.com","/") # example.com/`
- `preappend_if_missing("example.com","https://") # https://example.com`
- `remove_if_end("demo.json", ".json") # demo`
- `remove_if_begin("demo.json", "file://./") # file://./demo.json`

### quote/double_quote

Always wrap text in quotes and escape what is inside, so the result is safe to embed in a shell command or a quoted literal.

- `quote`: shell single-quote, inner `'` becomes `'\''` - `quote("it's") # 'it'\''s'`, e.g. `system("ls " quote(f))`
- `double_quote`: inner `"` and `\` are escaped with a backslash - `double_quote("say \"hi\"") # "say \"hi\""`

### parse/rparse

- parse: use wild match - `parse("Hello World","{greet} {name}")["greet"]`
- rparse: use regex group - `rparse("Hello World","(\\w+) (\\w+)")[1]`

### format_bytes/to_bytes

Convert bytes to human-readable format, and vice versa. Units(case-insensitive):
`B`, `KB`, `MB`, `GB`, `TB`, `PB`, `EB`, `ZB`, `YB`, `kib`, `mib`, `gib`, `tib`, `pib`, `eib`, `zib`, `yib`.

- `format_bytes(1024)`: 1 KB
- `to_bytes("2 KB")`: 2024

### mkpass

Generate password with numbers, lowercase/uppercase letters, and special chars.

- `mkpass()`: 8 chars password
- `mkpass(12)`: 12 chars password

### figlet

Help you to generate ASCII art text with figlet: `BEGIN { print figlet("Hello zawk"); }`.

**Attention**: ascii characters only, don't use i18n characters. :)

# Text Escape

- escape: `escape("format", $1)`:  support `json`, `csv`, `tsv`, `xml`, `html`, `sql`, `shell`
- escape_csv(s): Returns s escaped as a CSV column per RFC 4180: adds quotes if s contains `"`, `,`, a line break or
  leading/trailing spaces, and doubles quotes. Line breaks are kept as is inside the quotes.
- escape_tsv(s): Returns s escaped as a TSV column: backslash, tab, newline and carriage return characters are
  replaced with \\, \t, \n and \r, the escapes that `-i tsv` reverses. `escape("tsv", s)` is the same.

# Text Parser

If you want to see the returned data structure, you can use the var_dump function, such
as `var_dump(semver("1.2.3-alpha.1+zstd.1.5.0"))`.

### URL

`url(url_text)` to parse url and return array with following fields:

- schema
- user
- password
- host
- port
- path
- query
- fragment

examples: `url("https://example.com/user/1")`, `url("jdbc:mysql://localhost:3306/test")`

### Data URL(MapStrStr):

`data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")`

- data
- mime_type
- encoding

### Command line(MapIntStr)

`shlex("ls -l")`, https://crates.io/crates/shlex

### Path(MapStrStr)

`path("./demo.txt")`

- exists: `0` or `1`
- full_path
- parent
- file_name
- file_stem
- file_ext
- content_type

### Semantic Versioning(MapStrStr):

`semver("1.2.3-alpha")`, `semver("1.2.3-alpha.1+zstd.1.5.0")`

array fields:

- major:
- minor
- patch
- pre
- build

### Pairs

Parse pairs text to array(MapStrStr), for example:

* URL query string `id=1&name=Hello%20World1`
* Trace Context tracestate: `congo=congosSecondPosition,rojo=rojosFirstPosition`
* Cookies: `pairs(cookies_text, ";", "=")`, such
  as: `_device_id=c49fdb13b5c41be361ee80236919ba50; user_session=qDSJ7GlA3aLriNnDG-KJsqw_QIFpmTBjt0vcLy5Vq2ay6StZ;`

Usage: `pairs("a=b,c=d")`, `pairs("id=1&name=Hello%20World","&")`,  `pairs("a=b;c=d",";","=")`.

**Tips**: if `pairs("id=1&name=Hello%20World","&")`, text will be treated as URL query string, and URL decode will
be introduced to decode the value automatically.

### Records

Prometheus/OpenMetrics text format, such as `http_requests_total{method="post",code="200"}`

Usage:

* `record("http_requests_total{method='post',code=200}")`
* `record("mysql{host=localhost user=root password=123456 database=test}")`
* `record("table1(id int, age int)")`: DB table design

### Message

A message(record with body) always contains name, headers and body, and text format is
like `http_requests_total{method="post",code="200"}(100)`

Usage:

* `message("http_requests_total{method='post',code=200}(100)")`
* `message("login_event{method='post',code=200}('xxx@example.com')")`

### Function invocation

Parse function invocation format into `IntMap<Str>`, and 0 indicates function name.

* `arr=func("hello(1,2,3)")`: `arr[0]=>hello`, `arr[1]=>1`
* `arr=func("welcome('Jackie Chan',3)")`: `arr[0]=>welcome`, `arr[1]=>Jackie Chan`

# ID generator

### uuid

uuid : `uuid()`, `uuid("v7")`

ID specs:

* length:  128 bits
* version: v4, v7, and default is v4.

### ulid

ulid: Universally Unique Lexicographically Sortable Identifier, please refer https://github.com/ulid/spec for detail.

`ulid() #01ARZ3NDEKTSV4RRFFQ69G5FAV`

ID specs:

* length: 128 bits

### tsid

[tsid](https://github.com/jakudlaty/tsid/): TSID generator `tsid()`

### snowflake

[Snowflake ID](https://en.wikipedia.org/wiki/Snowflake_ID) is a form of unique identifier used in distributed computing.

`snowflake(machine_id)`, and max value for `machine_id` is `65535`.

ID specs:

* length: 64 bits
* machine_id: 16 bits, and max value is  `65535`;

# Array functions

### length

`length(arr)`

### delete

- delete item: `delete arr[1]`
- delete array: `delete arr`

### seq

`seq(start, end, step)`: `seq` command compatible

### uniq

`uniq(arr)`: IntMap<Str> -> IntMap<Str>, `uniq` command compatible

### asort

`n = asort(arr)`: sort array, and return sorted array length

### asorti

`n = asorti(src[, dest[, how]])`: sort the indices of `src` into `dest` (default: in place, replacing `src`), indexed from 1,
and return their number. `how` is one of gawk's predefined orders (`"@ind_str_asc"` by default, `"@ind_num_desc"`,
`"@val_num_asc"`, ...); user-defined comparison functions are not supported.

```awk
{ count[$1]++ }
END { n = asorti(count, keys); for (i = 1; i <= n; i++) print keys[i], count[keys[i]] }
```

### _max/_min/_sum/_mean

Max, min, sum and mean of the values of any array (string values are converted to numbers, keys may be strings); 0 for an empty array.
`_max`/`_min`/`_sum` return an integer for an integer array (e.g. `a[NR] = $1 + 0` with integral values), otherwise a float; `_mean` always returns a float.

```awk
{ bytes[$1] += $2; latency[NR] = $3 }
END { print _sum(bytes), _max(latency), _min(latency), _mean(latency) }
```

### _median/_quantile/_var/_stddev/_mode

Exact statistics over the values of any array (string values are converted to numbers, keys may be strings); 0 for an empty array.

* `_median(arr)`: median
* `_quantile(arr, q)`: q-quantile, `0 <= q <= 1`, interpolated linearly between ranks (as R's type 7 and numpy)
* `_var(arr)`, `_stddev(arr)`: sample variance and standard deviation (divided by `n - 1`)
* `_mode(arr)`: most frequent value, the smallest one on a tie

```awk
{ latency[NR] = $3 }
END { print _median(latency), _quantile(latency, 0.99), _stddev(latency) }
```

### _count_distinct/_top

* `_count_distinct(arr)`: number of distinct values of an array
* `_top(arr, k)`: the `k` keys with the largest numeric values, from the largest down, as an array indexed from 1; keys with equal values are ordered by key

```awk
{ count[$1]++ }
END { t = _top(count, 10); for (i = 1; i <= length(t); i++) print t[i], count[t[i]] }
```

### _join

`_join(arr, ",")` IntMap -> Str

### parse_array

`parse_array("['first','second','third']")`: IntMap<Str>

### tuple

`tuple("(1,2,'first','second')")`: IntMap<Str>

### variant

`variant("week(5)")`: StrMap<Str>

### flags

`flags("{vip,top20}")`: StrMap<Int>

### bloom filter

* `bf_insert(item)` or `bf_insert(item, group)`
* `bf_contains(item)` or `bf_contains(item, group)`
* `bf_icontains(item)` or `bf_icontains(item, group)`: Insert if not found. It's useful for duplication check.

Find unique phone numbers: `!bf_iconatins(phone) { }`

# Math

Floating-point operations: `sin(x)`, `cos(x)`, `atan(x)`, `atan2(y, x)`, `log(x)`, `log2(x)`, `log10(x)`, `sqrt(x)`,
`exp(x)` are delegated to the Rust standard library, or LLVM intrinsics where available.

### intdiv0(x, y)

Integer division truncated toward zero: `intdiv0(7, 3)` is `2`, `intdiv0(-7, 3)` is `-2`.
Returns `0` instead of failing when `y` is `0`.

### rand()

Returns a uniform random floating-point number between 0 and 1.

### srand(x)

Seeds the random number generator used by rand, returns the old seed.

### Bitwise operations

All of these operations coerce their operands to integers before being evaluated.

- `and(x, y)`, `or(x, y)`, `xor(x, y)`: bitwise and/or/xor
- `compl(x)`: bitwise complement
- `lshift(x, n)`: shift left, `rshift(x, n)`: arithmetic shift right, `rshiftl(x, n)`: logical shift right

### abs

`abs(-1) # 1`,

### floor

`floor(4.5) # 4`

### ceil

`ceil(4.5) # 5`

### round

`round(4.4) # 4`,

### eval

`eval("1+2")` or `eval("a + 2", context)`, and return type is Float.

Please refer https://github.com/isibboi/evalexpr for more.

**Attention**: Now only Int/Float/Boolean are supported, and boolean will be converted to 0/1.

### fend

`fend("1+2") # 3`

Please refer https://github.com/printfn/fend for more.

### min/max

`min(1,2,3)`, `max("A","B")`,

### bool

the return value is `0` or `1` for `mkbool(s)`.

examples: `mkbool("true")`, `mkbool("false")`, `mkbool("1")`, `mkbool("0")`, `mkbool("0.0")` `mkbool("  0  ")`,
`mkbool("Y")`, `mkbool("Yes")`, `mkbool("")`,
`mkbool("✓")`

### int/float

`int("11") # 11`,
`float("11.2") # 11.2`

`float()` reads decimal numbers and hexadecimal numbers with a `0x` prefix (and an optional sign), the same way
as every other string-to-number conversion: `float("0x11")` and `"0x11" + 0` are both `17`, `float("-0x11")` is `-17`.
A leading `0` alone is decimal: `float("017")` is `17`. There are no octal or binary prefixes: `float("0b101")` is `0`.

# Date/Time

utc by default.

### systime

`systime()`: current Unix time

### strftime

https://docs.rs/chrono/latest/chrono/format/strftime/index.html

`strftime([format [, timestamp [, utc]]])`, as in gawk:

* `strftime("%Y-%m-%d %H:%M:%S")`: the current time.
* `strftime("%Y-%m-%d", 0)`: a unix timestamp; negative timestamps are dates before 1970.
* `strftime("%H:%M", 0, 1)`: in UTC when the third argument is nonzero or a nonempty string, local time otherwise.
* `strftime()`: the current time in the format of `PROCINFO["strftime"]` (`%a %b %e %H:%M:%S %Z %Y` by default).
* `strftime("%+")`: ISO 8601 / RFC 3339 date & time format.

`%Z` prints the UTC offset of the time zone (`+08:00`) rather than its abbreviation.

### mktime

please refer https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats

- `mktime("2012 12 21 0 0 0")`:
- `mktime("2019-11-29 08:08-08")`:
- `mktime("1970 01 02 00 00 00", 1)`: `86400`, the 2nd argument is gawk's utc-flag: nonzero to parse text without an explicit offset as UTC, local time otherwise.
- `mktime("2024-01-01 10:00:00", 0, 8)`: `1704074400`, the 3rd argument is the UTC offset in hours (e.g. `8`, `-5`) for text without an explicit offset, and wins over the utc-flag.

`mktime()` returns `-1` if the text cannot be parsed, as gawk does.
The gawk format `"YYYY MM DD HH MM SS [DST]"` is tried first, and out-of-range values are normalized (e.g. month `13`).

### Duration

Convert duration to seconds: `duration("2min + 12sec") # 132`. Time
units: `sec, secs`, `min, minute, minutes`, `hour, h`, `day, d`, `week, wk`, `month, mo`, `year, yr`.

* Nushell Durations: https://www.nushell.sh/book/types_of_data.html#durations
* Fend: https://github.com/printfn/fend/blob/main/core/src/units/builtin.rs

# Color

Convert between hex and rgb.

- `hex2rgb("#FF0000") # [255,0,0]`: result is array `[r,g,b]`
- `rgb2hex(255,0,0) # #FF0000`

# Fake

Generate fake data for testing: `fake("name")` or `fake("name","cn")`.

* locale: `EN`(default) and `CN` are supported now.
* data: `name`, `phone`, `cell`, `email`, `wechat`, `ip`, `creditcard`, `zipcode`, `plate`, `postcode`, `id`(身份证).

# Decision(jev)

Typed decision with [TypeSafe System One API](https://docs.typesafe.ai/api), and `TYPESAFE_API_KEY` environment variable is required.
The first argument is a text or an array, and an array is sent as a JSON object.

* filter: `jev(text_or_array, "the name is European")`: 1 or 0, 1 when the probability of yes is above `jev_threshold`
  (awk variable, then `JEV_THRESHOLD` environment variable, default `0.5`).
* rank: `jev_prob(text_or_array, "the customer is angry")`: probability of yes, from 0 to 1.
* classify: `jev_choice(text_or_array, "which team should handle this?", options)`: the picked option.
* score: `jev_score(text_or_array, "how luxurious is this product?", levels)`: probability-weighted position on the levels, from 0 to `length(levels) - 1`.

Options/levels are an array, such as `split("budget,mid-range,premium,luxury", levels, ",")`, or a comma separated text.
For an array entry whose key and value differ (not an index), the value is the description of the key, e.g. `opts["security"] = "account access, passwords"`.
Levels of `jev_score` are ordered by index, so use an indexed array or a text for them.
Failed requests are reported to stderr, and the functions return `0`, `-1` or empty text.

```shell
zawk 'jev($0, "the name is European")' names.txt
```

You can use state object(array) instead of text:  

```shell
zawk 'BEGIN{ state["city"] = "Peking";  print jev(state, "the capital of China or not?") }'
```

# JSON

### from_json

`from_json(json_text)`: parse a JSON object (keys) or array (keys `1..n`) into an array.
Strings are decoded, `true`/`false` become `1`/`0`, `null` becomes `""`, nested arrays/objects stay as JSON text.

### to_json

`to_json(array)`: arrays with integer keys become JSON arrays (ordered by key), other arrays become JSON objects
with keys sorted, so the output is stable across runs. Empty values are kept. Values follow the array's inferred type:
a string-valued array (e.g. `a["n"]=1; a["s"]="x"`) encodes every value as a JSON string (`{"n":"1","s":"x"}`).

### json_value

`json_value(json_text, json_path)`: return only one text value - `json_value(json_text, '$.store.book[0].title')`.
Strings are decoded (escapes resolved, no quotes), `null` becomes `""`, numbers/booleans/arrays/objects are returned as JSON text.

**Tips**: [RFC 9535 JSONPath: Query Expressions for JSON](https://www.rfc-editor.org/rfc/rfc9535.html)

### json_query

`json_query(json_text, json_path)`: return array with text value, decoded with the same rules as `json_value`

### json_has

`json_has(json_text, key)`: 1 if the JSON object has the top-level key, else 0 - `json_has(json_text, "id")`.

### json_get

`json_get(json_text, key)`: value of the top-level key, `""` if not found, decoded with the same rules as `json_value` - `json_get(json_text, "name")`.

### json_set

`json_set(json_text, key, value)`: new JSON text with the key set - `json_set(json_text, "age", 18)`.
A value that is valid JSON text (`18`, `true`, `null`, `"text"`, `[1,2]`, `{"a":1}`) is kept as JSON, any other text is stored as a JSON string.
Empty `json_text` starts a new object; text that is not a JSON object is returned unchanged.

### json_del

`json_del(json_text, key)`: new JSON text without the key - `json_del(json_text, "password")`.

# CSV

### from_csv

`from_csv(csv_row)`: array of text value for one rows

### to_csv

`to_csv(array)`: csv row

# XML

### xml_value

`xml_value(xml_text, xpath)`: node's inner_text

**Attention**: Please refer [XPath cheatsheet](https://quickref.me/xpath.html) for xpath syntax.

### xml_query

`xml_query(xml_text, xpath)`: array of element's string value

# HTML

### html_value

`html_value(html_text, selector)`: node's inner_text

**Attention**: please follow standard CSS selector syntax.

### html_query

`html_query(html_text, selector)`: array of node's inner_text

# Encoding/Decoding

`encode("format",$1) `, and `decode("format",$1)` for the reverse.

Formats:

- `hex`,
- `base32`(RFC4648 without padding),
- `base58`
- `base62`
- `base64`,
- `base64url`: url safe without pad
- `zlib2base64url`: zlib then base64url, good for online diagram service, such
  as [PlantUML](https://plantuml.com/), [Kroki](https://kroki.io/)
- `url`,
- `hex-base64`,
- `hex-base64url`,
- `base64-hex`,
- `base64url-hex`

# Crypto

### Digest

`digest("algorithm",$1)`, `hash("algorithm",$1)` is an alias.

Algorithms:

- `md5`
- `sha256`,
- `sha512`,
- `bcrypt`,
- `murmur3`,
- `xxh32` or `xxh64`
- `blake3`
- `crc32`: checksum
- `adler32`: checksum

### crypto

- hmac: `hmac("HmacSHA256","your-secret-key", $1)`, `hmac("HmacSHA384", ...)` or `hmac("HmacSHA512", ...)`. Algorithm names are case-insensitive and the `Hmac` prefix is optional (`sha256`). Unsupported algorithms print a warning and return an empty string.
- encrypt:  `encrypt("aes-128-cbc", "Secret Text", "your_pass_key")`,
  `encrypt("aes-256-gcm", "Secret Text", "your_pass_key")`
- decrypt:  `decrypt("aes-128-cbc", encrypted_text, "your_pass_key")`

Explain for `encrypt` and `decrypt`:

* mode — Encryption mode, only `aes-128-gcm`, `aes-256-gcm`, `aes-128-cbc`, `aes-256-cbc` are supported (case-insensitive);
  any other mode is an error: a warning, `ERRNO` is set and the result is empty.
  **Use GCM** (`aes-256-gcm` recommended): it is authenticated, a tampered ciphertext fails to decrypt.
  CBC is kept for compatibility: since `v4:` it is encrypt-then-MAC (HMAC-SHA256), so a tampered ciphertext also fails to decrypt.
* plaintext — Text that need to be encrypted.
* key — Password of any length. The AES key is derived with PBKDF2-HMAC-SHA256 (100,000 iterations) and a random 16 bytes salt.

Output format is `v3:` + Base64(salt + nonce + ciphertext) for GCM, and `v4:` + Base64(salt + IV + ciphertext + HMAC-SHA256 tag)
for CBC (the tag is checked before decryption); a random IV/nonce is used for every call,
and the random salt is generated once per password per run (so PBKDF2 runs once, not for every record).
`decrypt` also accepts older formats: CBC `v3:` (no MAC, a warning is printed), `v2:` (PBKDF2 with a fixed salt) and legacy ciphertext without prefix
(key = password bytes truncated/zero padded to 16 or 32 bytes, deprecated: a warning is printed, please re-encrypt it).
`decrypt` returns an empty string when the input is malformed, the key is wrong or the plaintext is not valid UTF-8.

### JWT

Hmac signature: `HS256`, `HS384`, `HS512`:

- jwt: `jwt("HS256","your-secret-key", payload_map)`
- dejwt: `dejwt("your-secret-key", token)`, return payload map.

RSA/ECDSA/EdDSA: `RS256`, `RS384`, `RS512`, `ES256`, `ES384`, `EdDSA`:

- jwt: `jwt("RS256", private_key_pem_text, payload_map)`
- dejwt: `dejwt(public_key_pem_text, token)`

JWK: `RS256`, `RS384`, `RS512`, `ES256`, `ES384`, `ES512`,

- dejwt: `dejwt("https://example.com/jwks.json#kid", token)`: please add kid as anchor.
  JWKS URL must use `https://`, and `http://` is only allowed for `localhost`/`127.0.0.1` for local test.

**Security**: the allowed algorithms are decided by the key, not by the `alg` in token header:
a PEM key only accepts `RS*`/`PS*`/`ES*`/`EdDSA`, a plain secret only accepts `HS*`,
and a JWK only accepts algorithms matching its key type and `alg`. An empty map is returned if verification fails.

**Claims**: `exp` and `nbf` are optional, and checked when present (with 60 seconds leeway):
an expired token, or one not valid yet, is rejected; a token without `exp` never expires.

**Errors**: on failure (bad signature, expired token, wrong key type, JWKS fetch error, non-https JWKS URL, ...)
`dejwt` prints a warning on stderr and sets `ERRNO`, e.g. `dejwt: invalid token: ExpiredSignature`:

```awk
ERRNO = ""; claims = dejwt("secret", token)
if (ERRNO != "") print "invalid JWT: " ERRNO
```

**Tips**: you can use https://jwkset.com/generate to generate JWK json and keys PEM text.

# KV

Key/Value Functions:

- `kv_get(namespace, key)`
- `kv_put(namespace, key, text)`
- `kv_delete(namespace, key)`
- `kv_clear(namespace)`

### KV with SQLite

namespace is SQLite db name, and db path is `$HOME/.awk/sqlite.db`.

examples: `kv_get("namespace1", "nick")`.

### KV with Redis

namespace is Redis URL: `redis://localhost:6379/namespace`, or `redis://localhost:6379/0/namespace`
namespace is key name for Hash data structure.
For TLS, use `rediss://host:6380/namespace` (`redis+tls://` is an alias).

A namespace with an unsupported `scheme://` (e.g. `mongodb://...`) is reported as a warning,
and is not stored in the local SQLite database.

`kv_get("redis://user:password@host:6379/db/namespace")`

### KV with NATS

namespace is NATS URL: `nats://localhost:4222/bucket_name`, please use `nats kv add bucket_name` to create bucket

`kv_get("nats://localhost:4222/bucket_name/nick")`

# Network

### HTTP

`http_get(url,headers)`, `http_post(url, body, headers)`.

you can ignore headers if not required.

response array:

- status: such as `200`,`404`. `0` means network error.
- text: response as text
- HTTP header names: response headers, such as `Content-Type`

**Attention**: If body is json text that starts with `{` or `[` and ends with `{` or `[`,
and `Content-Type = application/json` will be added as HTTP header by default.

### email

`send_mail(from, to, subject, body[, smtp_url])`: send email, and `to` could be multiple emails separated by `,`.
Failures are reported as warnings on stderr.

The transport is chosen in this order:

- `smtp_url` argument: send by SMTP
- `SMTP_URL` environment variable: send by SMTP
- `MLSN_API_KEY` environment variable: API key for [MailerSend](https://www.mailersend.com/)
- `RESEND_API_KEY` environment variable: API key for [Resend](https://resend.com/)

Deprecated aliases: `send_email()` is the same as `send_mail()`, and `smtp_send(smtp_url, from, to, subject, body)`
is the same as `send_mail(from, to, subject, body, smtp_url)`.

SMTP URL format:

- SMTP basic: `smtp://localhost:1025`
- SMTP with auth: `smtp://user:password@host:25`
- SMTP + TLS `smtps://user:password@host:465`

### S3

- `s3_get(bucket, object_name)`: get object, and return value is text.
- `s3_put(bucket, object_name, body)`: put object, and body is text

Environment variables for S3 access:

- S3_ENDPOINT, OPENDAL_S3_ENDPOINT
- S3_ACCESS_KEY_ID, OPENDAL_S3_ACCESS_KEY_ID
- S3_ACCESS_KEY_SECRET, OPENDAL_S3_SECRET_ACCESS_KEY
- S3_REGION, OPENDAL_S3_REGION

The AWS style names `AWS_ENDPOINT_URL`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and `AWS_REGION` are also supported.
Input files can be read from S3 directly: `zawk -i csv '{print $1}' s3://bucket1/demo.csv`.

### NATS

Publish events to NATS: `publish("nats://host:4222/topic", body)`

### MQTT

Publish events to MQTT: `publish("mqtt://servername:1883/topic", body)`

- CloudFlare Pub/Sub: `mqtts://BROKER_TOKEN@YOUR-BROKER.YOUR-NAMESPACE.cloudflarepubsub.com/topic`

### local_ip

`local_ip() # 192.168.1.3`

# Database

All database functions take an optional third argument `params`, an array of positional parameters
bound to a prepared statement in key order (`arr[1]`, `arr[2]`...), so values containing `'` work
and untrusted input cannot inject SQL. **Do not build SQL by concatenating field values.**

```awk
{
  params[1] = $1; params[2] = $2
  sqlite_execute("app.db", "insert into users(id, name) values(?, ?)", params)
}
```

Placeholders: `?` or `?1` for SQLite, `?` for MySQL, `$1`, `$2`... for PostgreSQL.
Parameters are passed as text: SQLite/MySQL convert them by column type, PostgreSQL converts them
to the inferred parameter type (bool, integers, floats, numeric, json, uuid, inet, bytea, date/time types;
empty value is NULL for non-text types). For other PostgreSQL types, cast in SQL, such as `$1::text::interval`.

Connections are cached per URL for the whole run. A PostgreSQL or Redis (`kv_*`) connection that was closed
(server restart, idle timeout, network failure) is reconnected once on the next call, so long-running input
such as `tail -f` keeps working. In parallel mode (`-p r`/`-p f`), calls to the same kind of database share one
lock and are run one at a time, so heavy database work does not speed up with more workers.

### SQLite

url: `sqlite.db` db path

- `sqlite_query("sqlite.db", "select nick,email,age from user")`:
  `sqlite_query("sqlite.db", "select nick,email,age from user")[1]`
- `sqlite_execute("sqlite.db", "update users set nick ='demo' where id = 1")`
- `split("demo,1", p, ","); sqlite_execute("sqlite.db", "update users set nick = ? where id = ?", p)`

### PostgreSQL

url: `postgresql://postgres:postgres@localhost/db_name`

- `pg_query(url, "select id, name from people")`,
- `pg_execute(url,"update users set nick ='demo' where id = 1")`
- `split("demo,1", p, ","); pg_execute(url, "update users set nick = $1 where id = $2", p)`

### MySQL

url: `mysql://root:123456@localhost:3306/test`

- `mysql_query(url, "select id, name from people")`,
- `mysql_execute(url,"update users set nick ='demo' where id = 1")`
- `split("demo,1", p, ","); mysql_execute(url, "update users set nick = ? where id = ?", p)`

# Data Time

utc by default.

functions:

- systime: current Unix time
- strftime: `strftime("%Y-%m-%dT%H:%M:%S")` https://docs.rs/chrono/latest/chrono/format/strftime/index.html
- mktime: `mktime("2021-05-01T01:17:02")` https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats

### Date time parse

Parse date time text to array: `datatime()`, `datetime(1621530000)["year"]`, `datetime("2020-02-02")["year"]`
datetime text format:

- systemd.time: https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html
- dateparser: https://github.com/waltzofpearls/dateparser

date/time array:

- year: 2024
- month: 1, 2
- monthday: 24
- hour
- minute
- second
- yearday
- weekday
- hour: 1-24
- althour: 1-12

# OS

- `whoami()`,
- `os()`,
- `arch()`,
- `os_family()`,
- `pwd()`,
- `user_home()`

### getenv

`getenv()` is a function for `ENVIRON["NAME"]` with default value: `getenv("NAME", "default value")`.

**Attention**: zawk reads `.env` file and injects them as environment variables by default

### system2

`system2(cmd)` is different from `system(cmd)`, and it will return array with `code`, `stdout`, `stderr`.

To capture output of a command, and ou can use `getline` and pipe to get the output:

```
function  get_output(command) {
    while (command | getline line) {
        lines[i++] = line
    }
    return lines
}
```

With new `system2` function, you can get the output directly:

```
    result = system2("curl ifconfig.me")
    println result["stdout"]
```

**Attention**: If you don't want to capture output, you can use `system` function.

# I/O

### File

- read file into text: `read_all(file_path)`, `read_all("https://example.com/text.txt")`
- write text into file: `write_all(file_path, text)`  Replace if file exists.

**Tips**: `read_all` reads a local file or an `http://`/`https://` URL as text. Compressed content is not
decompressed, and other schemes (`ftp://`, `s3://`, ...) are not supported. If the file can't be read
(missing file, invalid UTF-8, network failure), a warning is printed and an empty string is returned.
The HTTP status is not checked: the body of an error response (404, ...) is returned as is.

### read_config

Read config file to StrStrMap: `read_config("tests/demo.ini")`.
Now only `*.ini` and `*.properties` are supported.

**Tips**: zawk will load `.env` as environment variables automatically if it exists in the current directory.

### close/fflush

- `close(name)`: close a file, command or pipe opened by redirection or `getline`, as in POSIX awk.
- `fflush([name])`: flush an output file or pipe, or all output if name is not provided.

### getline

Please visit: https://www.gnu.org/software/gawk/manual/html_node/Getline.html
and http://awk.freeshell.org/AllAboutGetline

# Misc

### Diagnose

- dump: `var_dump(name)`, `pprint(name)` is an alias.
- logging: `log_debug(msg)`, `log_info()`, `log_warn()`, `log_error()`

**Attention**: dump/logging output will be directed to std err to avoid std output pollution.

Logs from AWK scripts use the `zawk` target (`zawk:<FILENAME>` while processing an input file) and are enabled
at debug level by default, while dependencies only report warnings.
Use `RUST_LOG` (also loaded from `.env`) to adjust it, for example:

* `RUST_LOG=zawk=warn`: only output `log_warn()` and `log_error()`
* `RUST_LOG=zawk=off`: disable logs from AWK scripts
* `RUST_LOG=reqwest=debug`: debug HTTP requests made by `http_get()`/`http_post()`

### Reflection

- `isarray(x)`,
- `typeof(x)` https://www.gnu.org/software/gawk/manual/html_node/Type-Functions.html

### zawk

- `version()`: return zawk version
- `exit([code])`: exit with code (`0` by default), END actions are still executed, as in POSIX awk.
- `assert(cond, msg)`: if `cond` is false (tested as by `if`), print `msg` to stderr and exit at once with code `2`.
  Unlike `exit 2`, the `END` block is not run (in serial and parallel mode alike), e.g. `{ assert(NF == 5, f"line {NR}: expect 5 fields, got {NF}") }`.
- `zawk functions [--json] [FILTER]`: list builtin functions with signatures, descriptions and examples.
- `zawk inspect [-i FORMAT] [-o table|markdown] [-n ROWS] [--max-rows N] FILE...`: look at a CSV, TSV, JSON Lines or Parquet file
  before writing a program for it: the schema (field number, name and type of each column; a CSV header of `name:TYPE` declares the type),
  the first rows (10 by default) and a summary of each column like DuckDB's `SUMMARIZE` (min, max, approx_unique, avg, std, q25, q50, q75,
  count, null_percentage). The format comes from `-i`, the file extension or the content; compressed files, URLs and S3 objects are supported.
  `-o markdown` prints Markdown tables, handy for AI agents.

# Credits

thanks to:

* DuckDB Functions: https://duckdb.org/docs/sql/functions/overview
* ClickHouse String Functions: https://clickhouse.com/docs/en/sql-reference/functions/string-functions
* Golang stdlib: https://pkg.go.dev/std
* Rust stdlib: https://doc.rust-lang.org/std/
* Deno stdlib: https://deno.land/std
* PHP stdlib: https://www.php.net/manual/en/book.strings.php
* sttr: https://github.com/abhimanyu003/sttr
* Java:
    - [commons-lang](https://commons.apache.org/proper/commons-lang/apidocs/org/apache/commons/lang3/StringUtils.html)
    - [SpringFramework StringUtils](https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/util/StringUtils.html)
