---
title: Scripts as Tools
sidebar_label: Scripts as tools
sidebar_position: 7
---

# Scripts as Tools

A zawk script can be a documented command-line tool, with a generated `--help`.

## Create a script

```shell
zawk init report      # creates report.awk from a template
```

Metadata comments describe the script:

```awk title="orders.awk"
#!/usr/bin/env -S zawk -f
# @desc Summarize orders by day
# @meta author linux_china
# @meta version 0.1.0
# @var tz time zone offset in hours
# @var currency? currency symbol (optional)

# input: 2026-10-01 10:00:00,1200.50
BEGIN { FS = ","; currency ??= "$" }
{ c[strftime("%Y-%m-%d", mktime($1, 0, tz + 0), 1)] += $2 }
END {
    PROCINFO["sorted_in"] = "@ind_str_asc"
    for (d, total in c) print d, f"{currency}{total:,.2f}"
}
```

| Tag | Meaning |
|-----|---------|
| `@desc` | description of the script |
| `@meta` | metadata such as `author` or `version` |
| `@var` | a variable set with `-v name=value`; `name?` is optional |
| `@env` | a required environment variable read with `ENVIRON["NAME"]` (or from `.env`): the script stops with an error when it is missing |

```shell
$ chmod +x orders.awk
$ ./orders.awk --help
orders.awk 0.1.0
linux_china
Summarize orders by day

USAGE: orders.awk -v tz=[value] -v currency?=[value] <input-file>

ARGS:
  [tz]  time zone offset in hours
  [currency?]  currency symbol (optional)

$ ./orders.awk -v tz=8 -v currency=¥ orders.csv
2026-10-01 ¥1,210.50
2026-10-02 ¥5.00
```

With `-f` in the shebang, the script accepts zawk options such as `-v`. Use `-E` instead when the script takes
only input files: every following argument is then an input file, even one starting with `-`.

## Libraries with `@include`

```awk title="lib/strings.awk"
function slug(s) { return kebab_case(trim(s)) }
```

```awk
@include "lib/strings"
{ print slug($0) }
```

Files are searched in `AWKPATH` (default `.:/usr/local/share/awk`), as `lib` then `lib.awk`, and loaded once.

## Keep scripts healthy

```shell
zawk fmt -w *.awk             # format in place
zawk fmt --check *.awk        # CI: exit 1 if a file is not formatted
zawk lint -f orders.awk       # JSON diagnostics, exit 1 on errors
zawk --lint -f orders.awk data.txt   # lint, then run
```

See [Tooling](../tooling.md).

## Programs in Markdown

`-f` runs the first ` ```awk ` (or `zawk`/`gawk`) code block of a Markdown file, so a README or a runbook can
be both documentation and program:

````markdown title="runbook.md"
# Find slow requests

Requests slower than one second:

```awk
$NF > 1000 { print $7, $NF " ms" }
```
````

```shell
zawk -f runbook.md access.log
```

## Call zawk functions from the shell

```shell
cawk() { zawk "BEGIN { print ${1} }" }
cawk 'uuid("v7")'
cawk 'digest("sha256", "hello")'
cawk 'strftime("%+", systime())'
```
