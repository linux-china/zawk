AWK Standard Library
====================

Standard library for AWK with text, math, crypto, kv, database, network etc.

# Text functions

### match(s, re)

if string s matches the regular expression in re. If s matches, the RSTART variable is set with the start of the
leftmost match of re, and RLENGTH is set with the length of this match.

### substr(s, i[, j])

The 1-indexed substring of string s starting from index i and continuing for the next j characters or until the end of s
if i+j exceeds the length of s or if s is not provided.

### sub(re, t, s)

Substitutes t for the first matching occurrence of regular expression re in the string s.

### gsub(re, t, s)

Like sub, but with all occurrences substituted, not just the first.

### index(haystack, needle)

The first index within haystack in which the string needle occurs, 0 if needle does not appear.

### split(s, m[, fs])

Splits the string s according to fs, placing the results in the array m. If fs is not specified then the FS variable is
used to split s.

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

numeric value(十进制) `strtonum("0x11")`.

### trim

Trim text with space by default. `trim($1)`.
Trim text with chars with `trim($1, "[]()")`

### truncate

`truncate($1, 10)` or `truncate($1, 10, "...")`

### capitalize/uncapitalize:

`capitalize("hello") # Hello` or `uncapitalize("Hello") # hello`

### camel_case

`camel_case("hello World") # helloWorld`

### kebab_case:

`kebab_case("hello world") # hello-world`

### snake_case

`snake_case("hello world") # hello_world`

### title_case

`title_case("hello world") # Hello World`

### isint

`isint("123")`

### isnum

`isnum("1234.01")`

### mask

`mask("abc@example.com")`, `mask("186612347")`

### pad

- pad:  `pad($1, 10, "*")` to `***hello**`, `pad_start($1, 10, "*")` to `***hello`, `pad_end($1, 10, "**")`
  to `hello***`,

### strcmp:

text compare `strcmp($1, $2)` return -1, 0, 1

### words

text to words: `words("hello world? 你好") # ["hello", "world", "你", "好"]`

### repeat

`repeat("*",3) # ***`

**Tips**: please regex express for `is_xxx()`、`contains()`、`start_with()`、`end_with()` functions.

### default_if_empty

Return default value if text is empty or not exist.

`default_if_empty("   ", "demo") # demo` or `default_if_empty(var_is_null, "demo") # demo`

### append_if_missing/prepend_if_missing

Add suffix/prefix if missing

- `append_if_missing("nats://example.com","/") # example.com/`
- `preappend_if_missing("example.com","https://") # https://example.com`

### quote/double_quote

quote/double text if not quoted/double quoted.

- `quote("hello world") # 'hello world'`
- `double_quote("hello world") # "hello world"`

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

Usage: `pairs("a=b,c=d")`, `pairs("id=1&name=Hello%20World","&")`,  `pairs("a=b;c=d",";","=")`.

**Tips**: if `pairs("id=1&name=Hello%20World","&")`, text will be treated as URL query string, and URL decode will
be introduced to decode the value automatically.

### Attributes

Prometheus/OpenMetrics text format, such as `http_requests_total{method="post",code="200"}`

Usage: 

* `attributes("http_requests_total{method=\"post\",code=\"200\"}")`
* `attributes("mysql{host=localhost user=root password=123456 database=test}")`

### Message

A message always contains name, headers and boy, and text format is like `http_requests_total{method="post",code="200"}(100)`

Usage:

* `message("http_requests_total{method=\"post\",code=\"200\"}(100)")`
* `message("login_event{method=\"post\",code=\"200\"}('xxx@example.com')")`

# ID generator

### uuid

uuid : `uuid()`, `uuid("v7")`

ID specs:

* length:  128 bits
* version: v4, v7, and default is v4.

### ulid

Please refer https://github.com/ulid/spec for detail.

`ulid() #01ARZ3NDEKTSV4RRFFQ69G5FAV`

ID specs:

* length: 128 bits

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

examples: `mkbool("true")`, `mkbool("false")`, `mkbool("1")`, `mkbool("0")`, `mkbool("0.0")` `mkbool("  0  ")`, `mkbool("Y")`, `mkbool("Yes")`, `mkbool("")`,`mkbool("✓")`

### int/float

`int("11") # 11`,
`float("11.2") # 11.2`

# JSON

### from_json

`from_json(json_text)`

### to_json

`to_json(array)`

# CSV

### from_csv

`from_csv(csv_text)`

### to_csv

`to_csv(array)`

# Encoding/Decoding

`encode("format",$1) `

Formats:

- `hex`,
- `base32`(RFC4648 without padding),
- `base64`,
- `base64url`,
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
- `xxh32`,
- `xxh64`,
- `blake3`
- `crc32`: checksum
- `adler32`: checksum

### crypto

- hmac: `hmac("HmacSHA256","your-secret-key", $1)` or `hmac("HmacSHA512","your-secret-key", $1)`
- jwt: `jwt("HS256","your-secret-key", arr)`, `dejwt("your-secret-key", token)`. algorithm: `HS256`, `HS384`, `HS512`.

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

- `sqlite_query("sqlite.db", "select nick,email,age from user")`: sqlite_query("sqlite.db", "select nick,email,age from
  user")[1]
- `sqlite_execute`

### MySQL

url: `mysql://root:123456@localhost:3306/test`

- `mysql_query(url, "select id, name from people")`,
- `mysql_execute(url,"update users set nick ='demo' where id = 1")`

# Data Time

utc by default.

functions:

- systime: current Unix time
- strftime: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
- mktime: https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats

### Date time parse

Parse date time text to array: `datatime()`, `datetime(1621530000)["year"]`, `datetime("2020-02-02")["year"]`
datetime text format: https://github.com/waltzofpearls/dateparser

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

- read file into text: `read_all(file_path)`
- write text info file: `write_all(file_path, text)`  Replace if file exits.

### getline

Please visit: https://www.gnu.org/software/gawk/manual/html_node/Getline.html
and http://awk.freeshell.org/AllAboutGetline

# Misc

### Diagnose

- dump: `var_dump(name)`,
- logging: `log_debug(msg)`, `log_info()`, `log_warn()`, `log_error()`

### Reflection

- `isarray(x)`,
- `typeof(x)` https://www.gnu.org/software/gawk/manual/html_node/Type-Functions.html

# Credits

thanks to:

* Golang stdlib: https://pkg.go.dev/std
* Rust stdlib: https://doc.rust-lang.org/std/
* Deno stdlib: https://deno.land/std
* PHP stdlib: https://www.php.net/manual/en/book.strings.php
* Java:
    - [commons-lang](https://commons.apache.org/proper/commons-lang/apidocs/org/apache/commons/lang3/StringUtils.html)
    - [SpringFramework StringUtils](https://docs.spring.io/spring-framework/docs/current/javadoc-api/org/springframework/util/StringUtils.html)
