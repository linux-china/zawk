FAQ
===========

# Why to create zawk?

[zawk](https://github.com/linux-china/zawk) is good tool created by Eli Rosenthal.
We just want to make AWK more powerful with standard library. `zawk = frawk + stdlib`.

Time flies, and we need a new Modern AWK to work with DuckDB, ClickHouse, S3, KV etc. for text processing.

# Why not just contribute to zawk?

zawk is a foundation to awk for syntax, types, lex etc.,
and zawk focuses to make AWK more powerful with standard library.
Now I'm not sure that developers will accept my changes to zawk, and zawk just experimental
work: `zawk = AWK + stdlib + Rust`.

zawk still good for text processing, embedded etc.,
and if possible I will contribute some work to zawk, for example:

* Upgrade to Rust 2021
* Upgrade to Clap 4.5
* Dependencies updated to latest
* gawk compatible: global variables(ENVIRON, PROCINFO) and functions(datetime etc.)

# zawk will fix some bugs in zawk?

Yes. Eli Rosenthal had much less time over the last 1-2 years to devote to bug fixes and feature requests for zawk,
and I will try my best to fix bugs in zawk.

# Any roadmap for zawk?

Now I'm not sure about the roadmap, but I will try my best to make zawk more powerful and easy to use.

* gawk compatible
* stdlib enhancement
* performance optimization
* UX: Installation, Usage, Documentation, Examples etc.

# What are limits with gawk?

zawk limits:

- No `BEGINFILE` and `ENDFILE` blocks
- `CONVFMT` and `OFMT` are not supported
- With the default `FS` and `RS`, `\r` separates fields when reading input, so CRLF line endings are not part of the last field (see *Carriage returns* in [overview.md](overview.md))

# How to query Apache Parquet?

Use `-i parquet`: `$1..$NF` are the top-level columns, `FI` maps column names to their index, and
`$0` is the row as a JSON object.

```shell
$ zawk -i parquet '$FI["age"] > 30 { print $FI["name"], $FI["city"] }' family.parquet
$ zawk -i parquet -o csv '{ print $1, $2 }' family.parquet > family.csv
$ cat family.parquet | zawk -i parquet '{ print $0 }'    # JSON Lines
```

Values are converted as with `-i jsonl`:

- numbers and strings as is, decimals exactly (`12345.67`), booleans as `1`/`0`, null as empty
- DATE, TIME and TIMESTAMP as text: `2024-01-15`, `13:45:30.5`, `2024-01-15 10:30:00` (UTC)
- UUID as `a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11`, other binary values as hex
- LIST, MAP, STRUCT and JSON columns as JSON text, e.g. `[1,2,3]`, `{"city":"Beijing"}`
- columns of a type that cannot be read (such as INTERVAL) are empty, with a warning

Supported compressions: SNAPPY, ZSTD, LZ4 (and uncompressed). Files compressed with GZIP or BROTLI
are reported as errors. Parquet keeps its metadata at the end of the file, so standard input is read
into memory: prefer file arguments for large files. With several files, columns are matched by name
with those of the first file.

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

You can add help information in awk file to make awk friendly.
Use `zawk init demo.awk`, example as following: 

```awk
#!/usr/bin/env zawk -f

# @desc this is a demo awk
# @meta author linux_china
# @meta version 0.1.0
# @var nick current user nick
# @var email current user email
# @env DB_NAME database name

```

then you can use `./demo.awk --help` to get help support.

- `@desc`: description for awk file
- `@meta`: metadata for script, such as `author`, `version` etc.
- `@var`: variable for script, `email?` means that the variable is optional. Access by `awk -v varName="$PWD" ' END {print varName}'`.
- `@env`: environment variable, access by `ENVIRON["USER"]`.

# call zawk function from command line

Create a function `cawk` to call zawk: 

```shell
cawk() { zawk "BEGIN{ print ${1} }" }
```

then call `cawk 'uuid()'` to get result.

# Run a program from a URL

`-f` also accepts an `http://` or `https://` URL, and the downloaded text is run as the program:

```shell
zawk -f https://example.com/scripts/report.awk data.txt
```

Only a successful response (status 2xx) is used: an error response such as `404 Not Found` stops
zawk with an error instead of running the error page.

Running code from the network has the same risks as `curl ... | sh`. zawk does not verify the
downloaded program, and AWK programs can run commands (`system()`, `print | "cmd"`), write files
and use the network functions of the standard library. So:

- Only use URLs you trust, and prefer `https://` (plain `http://` can be modified in transit).
- The content behind a URL can change; for reproducible runs, pin a version (e.g. a tag or
  commit in the URL) or download the program, review it, and run the local file.
