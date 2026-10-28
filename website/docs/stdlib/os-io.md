---
title: OS, I/O & Misc
sidebar_label: OS, I/O & Misc
---

# OS, I/O & Misc

## Environment and OS

```awk
getenv("DB_URL", "sqlite://./app.db")   # ENVIRON with a default
ENVIRON["HOME"]
whoami(); os(); os_family(); arch(); pwd(); user_home()
```

zawk loads `.env` in the current directory as environment variables by default.

## Running commands

```awk
code = system("gzip -k " quote(FILENAME))       # exit code
r = system2("curl -s ifconfig.me")              # array: code, stdout, stderr
print r["stdout"]
"date +%F" | getline today                      # classic AWK pipe
print $0 | "sort -u"
```

As in gawk, commands are not restricted: run untrusted programs or input inside an operating-system sandbox,
and always `quote()` values from input.

## Files

```awk
text = read_all("notes.txt")               # also http:// and https:// URLs
write_all("out.txt", text)                 # replace the file
conf = read_config("app.ini")              # *.ini and *.properties
close(name); fflush(); fflush(name)
```

## Diagnostics and logging

```awk
var_dump(arr)                 # pprint() is an alias; output goes to stderr
log_debug("x = " x); log_info("starting"); log_warn("slow"); log_error("failed: " ERRNO)
```

Logs use the `zawk` target and can be tuned with `RUST_LOG` (also from `.env`):
`RUST_LOG=zawk=warn`, `RUST_LOG=zawk=off`, or `RUST_LOG=reqwest=debug` to debug HTTP calls.

## Reflection and runtime

```awk
typeof(x); isarray(x)          # as in gawk
version()                      # zawk version
exit 1                         # END still runs
assert(NF == 5, "bad record")  # exit 2 at once, END does not run
```

## Colors

```awk
hex2rgb("#FF0000")     # array [255, 0, 0]; #rgb shorthand is expanded
rgb2hex(255, 0, 0)     # #FF0000
```
