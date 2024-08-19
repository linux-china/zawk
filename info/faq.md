FAQ
===========

# Why to create zawk?

[frawk](https://github.com/ezrosent/frawk) is good tool created by Eli Rosenthal.
We just want to make AWK more powerful with standard library. `zawk = frawk + stdlib`.

Time flies, and we need a new Modern AWK to work with DuckDB, ClickHouse, S3, KV etc. for text processing.

# Why not just contribute to frawk?

frawk is a foundation to zawk for syntax, types, lex etc.,
and zawk focuses to make AWK more powerful with standard library.
Now I'm not sure that developers will accept my changes to frawk, and zawk just experimental
work: `zawk = AWK + stdlib`.

Frawk still good for text processing, embedded etc.,
and if possible I will contribute some work to frawk, for example:

* Upgrade to Rust 2021
* Upgrade to Clap 4.5
* Dependencies updated to latest
* gawk compatible: global variables(ENVIRON, PROCINFO) and functions(datetime etc.)

# zawk will fix some bugs in frawk?

Yes. Eli Rosenthal had much less time over the last 1-2 years to devote to bug fixes and feature requests for frawk,
and I will try my best to fix bugs in frawk.

# Any roadmap for zawk?

Now I'm not sure about the roadmap, but I will try my best to make zawk more powerful and easy to use.

* gawk compatible
* stdlib perfect
* performance optimization
* UX: Installation, Usage, Documentation, Examples etc.

# How to query Apache Parquet?

```shell
$ duckdb -c "COPY (select * from 'family.parquet') TO 'family.csv' (FORMAT CSV)"
```

# Special types in text

* bool:  `mkbool("true")`
* Tuple: `tuple("('abc',123)")`: IntMap<Str>
* Array: `parse_array("[1,2,3]")`: IntMap<Str>
* Record: `record("{field1:1,field2:'two'}")`: StrMap<Str>
* variants: `days(30)`, `week(2)`: StrMap<Str>, and key is `name` and `value`.
* flags: `{read,write}`: StrMap<Int>

You can use above functions to parse special types in text. 
If possible, don't add space in value text. 

**Tips**: No matter what type you use, the format should be regular expression friendly.

# Nushell integration

Please use `to csv` then pipe output to `zawk` for csv processing.

```shell
$ ls | to csv  | ^zawk -i csv '{print $1}'
```

Nushell types support:

* duration: `duration("2min + 32sec")`
* timestamp: `mktime("2024-04-27 17:07:25.684184848 +08:00")`
* lists: `parse_array("[0 1 'two' 3]")`
* file size: `to_bytes("1.5GB")`
* records: `record("{name:'Nushell', lang: 'Rust'}")`

# awk file help support

You can add help information in awk file to make awk friendly, example as following: 

```awk

#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta version 0.1.0
# @var nick current user nick
# @var email current user email
# @env DB_NAME database name

```

then you can use `zawk -f demo.awk --help` to get help support.

- `@desc`: description for awk file
- `@meta`: metadata for script, such as `author`, `version` etc.
- `@var`: variable for script, `email?` means that the variable is optional. Access by `awk -v varName="$PWD" ' END {print varName}'`.
- `@env`: environment variable, access by `ENVIRON["USER"]`.