---
title: Text Parsers
sidebar_label: Parsers
---

# Text Parsers

Real-world text is full of small structured values: URLs, query strings, cookies, semantic versions,
Prometheus labels, command lines. zawk parses them into arrays, so you can index the result directly.

Use `var_dump(x)` (alias `pprint`) to look at the structure of any result on stderr:

```shell
zawk 'BEGIN { var_dump(semver("1.2.3-alpha.1+zstd.1.5.0")) }'
```

## URL

`url(text)` returns `schema`, `user`, `password` (percent-decoded), `host`, `port`, `path`, `query` and `fragment`.

```shell
zawk '{ u = url($1); print u["host"], u["path"], pairs(u["query"], "&")["id"] }' urls.txt
```

```awk
url("jdbc:mysql://localhost:3306/test")["port"]     # 3306
data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")["data"]   # also mime_type, encoding
```

## Pairs: query strings, cookies, tracestate

```awk
pairs("a=b,c=d")["c"]                              # d
pairs("id=1&name=Hello%20World", "&")["name"]      # Hello World (URL-decoded with "&")
pairs(cookies, ";", "=")["user_session"]
```

## Records and messages

Prometheus/OpenMetrics-like text becomes an array:

```awk
r = record("http_requests_total{method='post',code=200}")
# r["method"] = "post", r["code"] = "200"
db = record("mysql{host=localhost user=root password=123456 database=test}")
m = message("login_event{method='post',code=200}('xxx@example.com')")   # name, headers and body
```

## Paths, versions and command lines

```awk
p = path("./demo.txt")       # exists, full_path, parent, file_name, file_stem, file_ext, content_type
v = semver("1.2.3-alpha")    # major, minor, patch, pre, build
args = shlex("ls -l 'my file'")    # args[1] = ls, args[2] = -l, args[3] = my file
```

## Literal values

| Function | Input | Result |
|----------|-------|--------|
| `parse_array(s)` | `"['first','second','third']"` | array indexed from 1 |
| `tuple(s)` | `"(1,2,'first','second')"` | array indexed from 1 |
| `func(s)` | `"welcome('Jackie Chan',3)"` | `[0]` = `welcome`, `[1]` = `Jackie Chan`, `[2]` = `3` |
| `variant(s)` | `"week(5)"` | `name` = `week`, `value` = `5` |
| `flags(s)` | `"{vip,top20}"` | `vip` = 1, `top20` = 1 |
| `mkbool(s)` | `"yes"`, `"off"`, `"✓"` | `1`, `0`, `1` |

These also cover [Nushell](https://www.nushell.sh/) values: `duration("2min + 32sec")`,
`parse_array("[0 1 'two' 3]")`, `to_bytes("1.5GB")`, `record("{name:'Nushell', lang: 'Rust'}")`.

## Configuration files

```awk
conf = read_config("app.properties")    # also *.ini
print conf["db.url"]
```

`.env` in the current directory is always loaded as environment variables: read it with `ENVIRON["NAME"]` or
`getenv("NAME", "default")`.
