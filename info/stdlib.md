AWK Standard Library
====================

Standard library for AWK with text, math, crypto, kv, database, network etc.

AWK stdlib Cheat Sheet: https://cheatography.com/linux-china/cheat-sheets/zawk/

# Text functions

Text is encoding with utf-8 by default.

### char_at

Get char at index: `char_at($1, 1)`, starts from 1. If index is out of range, return empty string.

### match(text, re)

if string text matches the regular expression in re. If s matches, the RSTART variable is set with the start of the
leftmost match of re, and RLENGTH is set with the length of this match.

### substr(text, i[, j])

The 1-indexed substring of string s starting from index i and continuing for the next j characters or until the end of s
if i+j exceeds the length of s or if s is not provided.

### sub(re, text, s)

Substitutes t for the first matching occurrence of regular expression re in the string s.

### gsub(re, text, s)

Like sub, but with all occurrences substituted, not just the first.

### index(haystack, needle)/last_index()

* `index()`: the first index within haystack in which the string needle occurs, 0 if needle does not appear.
* `last_index()`: the last index within haystack in which the string needle occurs, 0 if needle does not appear.

### split(text, arr[, fs])

Splits the string s according to fs, placing the results in the array `arr`. If fs is not specified then the FS variable is
used to split s.

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

### hex(s)

Returns the hexadecimal integer (e.g. 0x123abc) encoded in s, or 0 otherwise.

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

numeric value(Decimal) `strtonum("0x11")`.

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

quote/double text if not quoted/double quoted.

- `quote("hello world") # 'hello world'`
- `double_quote("hello world") # "hello world"`

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
- escape_csv(s): Returns s escaped as a CSV column, adding quotes if necessary, replacing quotes with double-quotes, and
  escaping other whitespace.
- escape_tsv(s): Returns s escaped as a TSV column. There is less to do with CSV, but tab and newline characters are
  replaced with \t and \n.

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

* `record("http_requests_total{method=\"post\",code=\"200\"}")`
* `record("mysql{host=localhost user=root password=123456 database=test}")`
* `record("table1(id int, age int)")`: DB table design

### Message

A message(record with body) always contains name, headers and body, and text format is
like `http_requests_total{method="post",code="200"}(100)`

Usage:

* `message("http_requests_total{method=\"post\",code=\"200\"}(100)")`
* `message("login_event{method=\"post\",code=\"200\"}('xxx@example.com')")`

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

`n = asort(arr)`: asort
gawk兼容

### _max/_min/_sum/_mean

`_max(arr)`: IntIntMap -> Int, IntFloatMap -> Float

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

Floating-point operations: sin, cos, atan, atan2, log, log2, log10, sqrt, exp are delegated to the Rust standard
library, or LLVM intrinsics where available.

### rand()

Returns a uniform random floating-point number between 0 and 1.

### srand(x)

Seeds the random number generator used by rand, returns the old seed.
Bitwise operations. All of these operations coerce their operands to integers before being evaluated.

### abs

`abs(-1) # 1`,

### floor

`floor(4.5) # 4`

### ceil

`ceil(4.5) # 5`

### round

`round(4.4) # 4`,

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

# Date/Time

utc by default.

### systime

`systime()`: current Unix time

### strftime

https://docs.rs/chrono/latest/chrono/format/strftime/index.html

* `strftime("%Y-%m-%d %H:%M:%S")`
* `strftime()` or `strftime("%+")`: ISO 8601 / RFC 3339 date & time format.

### mktime

please refer https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats

- `mktime("2012 12 21 0 0 0")`:
- `mktime("2019-11-29 08:08-08")`:

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

# JSON

### from_json

`from_json(json_text)`

### to_json

`to_json(array)`

### json_value

`json_value(json_text, json_path)`: return only one text value

**Tips**: [RFC 9535 JSONPath: Query Expressions for JSON](https://www.rfc-editor.org/rfc/rfc9535.html)

### json_query

`json_query(json_text, json_path)`: return array with text value

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

`encode("format",$1) `

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

`digest("algorithm",$1)`

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

- hmac: `hmac("HmacSHA256","your-secret-key", $1)` or `hmac("HmacSHA512","your-secret-key", $1)`
- jwt: `jwt("HS256","your-secret-key", arr)`. algorithm: `HS256`, `HS384`, `HS512`.
- dejwt: `dejwt("your-secret-key", token)`.
- encrypt:  `encrypt("aes-128-cbc", "Secret Text", "your_pass_key")`,
  `encrypt("aes-256-gcm", "Secret Text", "your_pass_key")`
- encrypt:  `decrypt("aes-128-cbc", "7b9c07a4903c9768ceeeb922bcb33448", "your_pass_key")`

Explain for `encrypt` and `decrypt`:

* mode — Encryption mode. now only `aes-128-cbc`, `aes-256-cbc`, `aes-128-gcm`, `aes-256-gcm` support
* plaintext — Text that need to be encrypted.
* key — Encryption key. `16` bytes(16 ascii chars) for `128` and `32` bytes(32 ascii chars) for `256`.

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

`kv_get("redis://user:password@host:6379/db/namespace")`

### KV with NATS

namespace is NATS URL: `nats://localhost:4222/bucket_name`, please use `nats kv add bucket_name` to create bucket

`kv_get("nats://localhost:4222/bucket_name/nick")`

# Network

### HTTP

`http_get(url,headers)`, `http_post(url, headers, body)`

response array:

- status: such as `200`,`404`. `0` means network error.
- text: response as text
- HTTP header names: response headers, such as `Content-Type`

### S3

- `s3_get(bucket, object_name)`: get object, and return value is text.
- `s3_put(bucket, object_name, body)`: put object, and body is text

Environment variables for S3 access:

- S3_ENDPOINT
- S3_ACCESS_KEY_ID
- S3_ACCESS_KEY_SECRET
- S3_REGION

### NATS

Publish events to NATS: `publish("nats://host:4222/topic", body)`

### local_ip

`local_ip() # 192.168.1.3`

# Database

### SQLite

url: `sqlite.db` db path

- `sqlite_query("sqlite.db", "select nick,email,age from user")`:
  `sqlite_query("sqlite.db", "select nick,email,age from user")[1]`
- `sqlite_execute("sqlite.db, "update users set nick ='demo' where id = 1")`

### libSQL

libSQL url: `./demo.db`, `http://127.0.0.1:8080` or `libsql://db-name-your-name.turso.io?authToken=xxxx`.

- `libsql_query(url, "select id, email from users")`,
- `libsql_execute(url,"update users set nick ='demo' where id = 1")`

**Tip**: If you don't want to put `authToken` in url, for example `libsql://db-name-your-name.turso.io`,
you can set up `LIBSQL_AUTH_TOKEN` environment variable.

### MySQL

url: `mysql://root:123456@localhost:3306/test`

- `mysql_query(url, "select id, name from people")`,
- `mysql_execute(url,"update users set nick ='demo' where id = 1")`

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

# I/O

### File

- read file into text: `read_all(file_path)`, `read_all("https://example.com/text.gz")`
- write text info file: `write_all(file_path, text)`  Replace if file exits.

**Tips**: `read_all` function uses [OneIO](github.com/bgpkit/oneio), and remote(https or ftp) and compressions(
gz,bz,lz,xz) are supported.

### getline

Please visit: https://www.gnu.org/software/gawk/manual/html_node/Getline.html
and http://awk.freeshell.org/AllAboutGetline

# Misc

### Diagnose

- dump: `var_dump(name)`,
- logging: `log_debug(msg)`, `log_info()`, `log_warn()`, `log_error()`

**Attention**: dump/logging output will be directed to std err to avoid std output pollution.

### Reflection

- `isarray(x)`,
- `typeof(x)` https://www.gnu.org/software/gawk/manual/html_node/Type-Functions.html

### zawk

- `version()`: return zawk version

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
