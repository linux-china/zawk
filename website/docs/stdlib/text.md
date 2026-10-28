---
title: Text Functions
sidebar_label: Text
---

# Text Functions

Text is UTF-8: positions and lengths count characters, not bytes.

## Length and characters

| Function | Example | Result |
|----------|---------|--------|
| `length(s)` | `length("你好")` | `2` |
| `strlen(s)` | `strlen("你好")` | `6` (bytes) |
| `char_at(s, i)` | `char_at("hello", 1)` | `h` (1-based, `""` out of range) |
| `chars(s)` | `chars("abc")` | array `a, b, c` indexed from 1 |
| `index(s, t)` / `last_index(s, t)` | `last_index("a/b/c", "/")` | `4` |
| `last_part(s[, sep])` | `last_part("a/b/c")` | `c` (tries `/`, then `.`) |

## Trimming, padding and shaping

```awk
trim("  hi  ")                 # "hi"
trim("[hello]", "[]")          # "hello": any of the chars, from both ends
truncate($1, 10)               # at most 10 chars
truncate($1, 10, "...")        # with a suffix
pad("hello", 10, "*")          # "***hello**"
pad_start("hello", 8, "*")     # "***hello"
pad_end("hello", 8, "*")       # "hello***"
repeat("*", 3)                 # "***"
wrap(text, 40)                 # word-wrap to 40 display columns (CJK counts as 2)
default_if_empty("  ", "demo") # "demo"
append_if_missing("nats://example.com", "/")   # "nats://example.com/"
prepend_if_missing("example.com", "https://")  # "https://example.com"
remove_if_end("demo.json", ".json")            # "demo"
remove_if_begin("file://demo.json", "file://") # "demo.json"
```

## Case conversion

```awk
capitalize("hello")         # Hello
uncapitalize("Hello")       # hello
camel_case("hello World")   # helloWorld
pascal_case("hello_world")  # HelloWorld
snake_case("hello world")   # hello_world
kebab_case("hello world")   # hello-world
title_case("NASA launch")   # NASA Launch
```

The four identifier styles agree on word boundaries: `HTTPServer` becomes `httpServer`, `HttpServer`,
`http_server` and `http-server`.

## Tests and comparison

```awk
starts_with($1, "https://")     # 1 or 0
ends_with($1, ".com")
contains($1, "//")
isint("123")                    # 1
isnum("1234.01")                # 1, same rules as strnum conversion
is("email", "demo@example.com") # formats: email, url, phone, ip
strcmp($1, $2)                  # -1, 0 or 1
similar("martha", "marhta")     # 0.961111 (Jaro-Winkler, 0..1)
```

Fuzzy matching with `similar` is one line:

```shell
zawk 'similar($1, "apple") > 0.8' fruits.txt
```

## Splitting and extracting

```awk
split("a=1; b=2", parts, /[=;]/, seps)       # gawk's 4-argument split
patsplit("a1b22c333", nums, /[0-9]+/)        # 3: nums = 1, 22, 333
gensub(/(a)(b)/, "\\2\\1", "g", "abab")      # "baba"
lines(text)                                  # array of non-empty lines
words("hello world? 你好")                   # hello, world, 你, 好
parse("Hello World", "{greet} {name}")["name"]       # wildcard template -> "World"
rparse("Hello World", "(\\w+) (\\w+)")[2]            # regex groups -> "World"
join_fields(2, NF, "-")                      # fields 2..NF joined by "-"
```

## Privacy, quoting and escaping

```awk
mask("abc@example.com")      # abc*********com
mask("13800138000")          # 13*******00
quote("it's")                # 'it'\''s'  -- safe in a shell command
double_quote("say \"hi\"")   # "say \"hi\""
escape("json", s)            # also: csv, tsv, xml, html, sql, shell
escape_csv(s); escape_tsv(s)
```

```awk
{ system("ls -l " quote($1)) }   # never pass raw input to a shell
```

## Numbers and sizes

```awk
format_bytes(1024)           # 1 KB
to_bytes("1.5GB")            # bytes as a number
hex("ff")                    # 255
strtonum("0x11")             # 17 (gawk compatible: "017" is octal 15)
mkpass(12)                   # random 12-char password
```

## Fun

```shell
$ zawk 'BEGIN { print figlet("zawk") }'
```
