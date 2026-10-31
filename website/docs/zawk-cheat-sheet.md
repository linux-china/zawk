---
title: zawk Cheat Sheet
sidebar_label: Cheat Sheet
date: 2026-10-10 15:18:34
background: bg-slate-600
tags:
  - bash
  - text
  - script
categories:
  - Linux Command
intro: |
  This is a one page quick reference cheat sheet to the [zawk](https://github.com/linux-china/zawk), which is a modern AWK implementation with additional features and improvements.
plugins:
  - copyCode
---

This is a one page quick reference cheat sheet to [zawk](https://github.com/linux-china/zawk), a modern AWK
implementation with a rich standard library and built-in support for structured data.

## Getting Started

### Have a try

```shell
$ zawk -F: '{print $1, $NF}' /etc/passwd
```

| -             | -                         |
|---------------|---------------------------|
| `-F:`         | Colon as a separator      |
| `-i csv`      | CSV input mode            |
| `-i parquet`  | Parquet input mode        |
| `{...}`       | Awk program               |
| `print`       | Prints the current record |
| `$1`          | First field               |
| `$NF`         | Last field                |
| `/etc/passwd` | Input data file           |

### Awk program

```
BEGIN          {<initializations>}
   <pattern 1> {<program actions>}
   <pattern 2> {<program actions>}
   ...
END            {< final actions >}
```

#### Example

```shell
zawk '
    BEGIN { print "\n>>>Start" }
    !/(login|shutdown)/ { print NR, $0 }
    END { print "<<<END\n" }
' /etc/passwd
```

### Variables

```text
          $1      $2/$(NF-1)    $3/$NF
           ▼          ▼           ▼
        ┌──────┬──────────────┬───────┐
$0/NR ▶ │  ID  │  WEBSITE     │  tag  │
        ├──────┼──────────────┼───────┤
$0/NR ▶ │  1   │  x.com       │   x   │
        ├──────┼──────────────┼───────┤
$0/NR ▶ │  2   │  google.com  │   g   │
        └──────┴──────────────┴───────┘
```

```shell
# First and last field
zawk -F: '{print $1, $NF}' /etc/passwd

# With line number
zawk -F: '{print NR, $0}' /etc/passwd

# Second last field
zawk -F: '{print $(NF-1)}' /etc/passwd

# Local variables
zawk 'BEGIN {local name="Jackie"; print "Hello " name}'

# f-string
zawk -F: '{print f"{$1} = {$(-2)}"}' /etc/passwd

# CSV with header
zawk -H -i csv '{print $FI["name"]}' demo.csv

# Parquet columns
zawk -i parquet '{print $FI["name"]}' demo.parquet
```

See: [Variables](#awk-variables)

### Awk program examples

```shell
zawk 'BEGIN {print "hello world"}'        # Prints "hello world"
zawk -F: '{print $1}' /etc/passwd         # -F: Specify field separator

# /pattern/ Execute actions only for matched pattern
zawk -F: '/root/ {print $1}' /etc/passwd

# BEGIN block is executed once at the start
zawk -F: 'BEGIN { print "uid"} { print $1 }' /etc/passwd

# END block is executed once at the end
zawk -F: '{print $1} END { print "-done-"}' /etc/passwd

# Read CSV file
zawk -H -i csv '{print $FI["name"]}' demo.csv

# Read Parquet file
zawk -i parquet '{print $FI["name"]}' demo.parquet

# Try to get a value from a function, and return a default value on failure
zawk 'BEGIN {value = try read_all("/no/such/file") else "none"; print value}'
```

```
# Read Parquet file from S3
zawk -i parquet '{print $FI["name"]}' s3://bucket1/family.parquet
```

### Conditions

```shell
zawk -F: '$3>30 {print $1}' /etc/passwd
```

See: [Conditions](#awk-conditions)

### Generate 1000 spaces

```shell
zawk 'BEGIN {print repeat(" ", 1000)}'
```

### Data Type

- Number: `111`, `11.11`, `0x90`
- String: `"hello"`
    - f-string: `f"Hello {name}"`
    - raw string: `r"C:\Program Files\Rust"`
- Associative Array: `fruits["mango"] = "yellow"`

See: [Types in zawk](https://github.com/linux-china/zawk/blob/master/info/types.md)

### Associative Arrays

```shell
zawk 'BEGIN {
   fruits["mango"] = "yellow";
   fruits["orange"] = "orange"
   for (name, color in fruits) {
     print f"The color of {name} is {color}"
   }
}'
```

See: [Arrays](#awk-arrays)

### Functions

```shell
# => 5
zawk 'BEGIN {print length("hello")}'
# => HELLO
zawk 'BEGIN {print toupper("hello")}'
# => hel
zawk 'BEGIN {print substr("hello", 1, 3)}'
```

See: [Functions](#awk-functions)

## Awk Variables

### Built-in variables

| -              | -                                                   |
|----------------|-----------------------------------------------------|
| `$0`           | Whole line                                          |
| `$1, $2...$NF` | First, second… last field                           |
| `$(-1)`        | Last field, same as `$NF`                           |
| `NR`           | `N`umber of `R`ecords                               |
| `NF`           | `N`umber of `F`ields                                |
| `OFS`          | `O`utput `F`ield `S`eparator <br /> _(default " ")_   |
| `FS`           | input `F`ield `S`eparator <br /> _(default " ")_      |
| `ORS`          | `O`utput `R`ecord `S`eparator <br /> _(default "\n")_ |
| `RS`           | input `R`ecord `S`eparator <br /> _(default "\n")_    |
| `FILENAME`     | Name of the file                                    |
| `FI`           | Column name to index, as in `$FI["name"]`, for CSV, JSON, Parquet etc. |

### Expressions

| -                        | -                                  |
|--------------------------|------------------------------------|
| `$1 == "root"`           | First field equals root            |
| `{print $(NF-1)}`        | Second last field                  |
| `NR!=1{print $0}`        | From 2nd record                    |
| `NR > 3`                 | From 4th record                    |
| `NR == 1`                | First record                       |
| `END{print NR}`          | Total records                      |
| `BEGIN{print OFMT}`      | Output format                      |
| `{print NR, $0}`         | Line number                        |
| `{print NR "\t" $0}`     | Line number (tab)                  |
| `{$1 = NR; print}`       | Replace 1st field with line number |
| `$NF > 4`                | Last field > 4                     |
| `NR % 2 == 0`            | Even records                       |
| `NR==10, NR==20`         | Records 10 to 20                   |
| `BEGIN{print ARGC}`      | Total arguments                    |
| `ORS=NR%5?",":"\n"`      | Concatenate records                |
| `name = name ?? "Guest"` | Default if empty                   |
| `name ??= "Guest"`       | Default if empty                   |

### Examples

Print sum and average

```shell
zawk -F: '{sum += $3}
     END { print sum, sum/NR }
' /etc/passwd
```

Printing parameters

```shell
zawk 'BEGIN {
    for (i = 1; i < ARGC; i++)
        print ARGV[i] }' a b c
```

Output field separator as a comma

```shell
zawk 'BEGIN { FS=":";OFS=","}
    {print $1,$2,$3,$4}' /etc/passwd
```

Position of match

```shell
zawk 'BEGIN {
    if (match("One Two Three", "Tw"))
        print RSTART }'
```

Length of match

```shell
zawk 'BEGIN {
    if (match("One Two Three", "re"))
        print RLENGTH }'
```

### Environment Variables

| -         | -                                                         |
|-----------|-----------------------------------------------------------|
| `ARGC`    | Number of arguments                                       |
| `ARGV`    | Array of arguments                                        |
| `FNR`     | `F`ile `N`umber of `R`ecords                              |
| `OFMT`    | Format for numbers <br /> _(default "%.6g")_                |
| `RSTART`  | Location in the string                                    |
| `RLENGTH` | Length of match                                           |
| `SUBSEP`  | Multi-dimensional array separator <br /> _(default "\034")_ |
| `ARGIND`  | Argument Index                                            |

### gawk and zawk only

| -             | -                                                        |
|---------------|----------------------------------------------------------|
| `ENVIRON`     | Environment variables                                    |
| `IGNORECASE`  | Ignore case                                              |
| `CONVFMT`     | Conversion format                                        |
| `ERRNO`       | System errors                                            |
| `PROCINFO["sorted_in"]` | Iteration order of `for...in`                  |
| `MSG_SUBJECT` | Message's subject when reading messages from NATS and Kafka |
| `MSG_HEADERS` | Message's headers                                        |
| `MSG_BODY`    | Message's body                                           |

### Defining variable

```shell
zawk -v var1="Hello" -v var2="World" '
    END {print var1, var2}
' </dev/null
```

#### Use shell variables

```shell
zawk -v varName="$PWD" '
    END {print varName}' </dev/null
```

## Awk Operators

### Operators

| -                    | -                |
|----------------------|------------------|
| `{print $1}`         | First field      |
| `$2 == "foo"`        | Equals           |
| `$2 != "foo"`        | Not equals       |
| `"foo" in array`     | In array         |
| `"foo" !in array`    | Not in array     |
| `foo = foo ?? "bar"` | Default if empty |
| `foo ??= "bar"`      | Default if empty |

`??` treats an uninitialized value, `""` and zero as empty.

#### Regular expression

| -               | -                 |
|-----------------|-------------------|
| `/regex/`       | Line matches      |
| `!/regex/`      | Line not matches  |
| `$1 ~ /regex/`  | Field matches     |
| `$1 !~ /regex/` | Field not matches |

#### More conditions

| -                        | -   |
|--------------------------|-----|
| `($2 <= 4 \|\| $3 < 20)` | Or  |
| `($1 == 4 && $3 < 20)`   | And |

### Operations

#### Arithmetic operations

`+` `-` `*` `/` `%` `++` `--`

#### Shorthand assignments

`+=` `-=` `*=` `/=` `%=`

#### Comparison operators

`==` `!=` `<` `>` `<=` `>=`

### Examples

```shell
zawk 'BEGIN {
    if ("foo" ~ "^fo+$")
        print "Fooey!";
}'
```

#### Not match

```shell
zawk 'BEGIN {
    if ("boo" !~ "^fo+$")
        print "Boo!";
}'
```

#### if in array

```shell
zawk 'BEGIN {
    assoc["foo"] = "bar";
    assoc["bar"] = "baz";
    if ("foo" in assoc)
        print "Fooey!";
}'
```

## Awk Functions

### Common functions

| Function              | Description                                                                     |
|-----------------------|---------------------------------------------------------------------------------|
| `index(s,t)`          | Position in string s where string t occurs, 0 if not found                      |
| `length(s)`           | Length of string s (or $0 if no arg)                                            |
| `rand()`              | Random number between 0 and 1                                                   |
| `substr(s,index,len)` | Return len-char substring of s that begins at index (counted from 1)            |
| `srand(seed)`         | Set seed for rand and return previous seed                                      |
| `int(x)`              | Truncate x to integer value                                                     |
| `split(s,a,fs)`       | Split string s into array a split by fs, returning length of a                  |
| `match(s,r)`          | Position in string s where regex r occurs, or 0 if not found                    |
| `sub(r,t,s)`          | Substitute t for first occurrence of regex r in string s (or $0 if s not given) |
| `gsub(r,t,s)`         | Substitute t for all occurrences of regex r in string s                         |
| `gensub(r,t,how,s)`   | Return s with the how-th (or `"g"` all) matches of r replaced by t              |
| `system(cmd)`         | Execute cmd and return exit status                                              |
| `tolower(s)`          | String s to lowercase                                                           |
| `toupper(s)`          | String s to uppercase                                                           |
| `getline`             | Set $0 to next input record from current input file                             |

See: [Standard Library](#standard-library), or run `zawk functions` to list all functions.

### User defined function

```shell
zawk '
    # Returns minimum number
    function find_min(num1, num2){
       if (num1 < num2)
       return num1
       return num2
    }
    # Returns maximum number
    function find_max(num1, num2){
       if (num1 > num2)
       return num1
       return num2
    }
    # Main function
    function main(num1, num2){
       result = find_min(num1, num2)
       print "Minimum =", result

       result = find_max(num1, num2)
       print "Maximum =", result
    }
    # Script execution starts here
    BEGIN {
       main(10, 60)
    }
'
```

## Awk Arrays

### Array with index

```shell
zawk 'BEGIN {
    arr[0] = "foo";
    arr[1] = "bar";
    print(arr[0]); # => foo
    delete arr[0];
    print(arr[0]); # => ""
}'
```

### Array with key

```shell
zawk 'BEGIN {
    assoc["foo"] = "bar";
    assoc["bar"] = "baz";
    print("baz" in assoc); # => 0
    print("foo" in assoc); # => 1
}'
```

### Array with split

```shell
zawk 'BEGIN {
    split("foo:bar:baz", arr, ":");
    for (key, value in arr)
        print value;
}'
```

### Array with asort

```shell
zawk 'BEGIN {
    arr[0] = 3
    arr[1] = 2
    arr[2] = 4
    n = asort(arr)
    for (i = 1; i <= n; i++)
        print(i, arr[i])
}'
```

### Multi-dimensional

```shell
zawk 'BEGIN {
    multidim[0,0] = "foo";
    multidim[0,1] = "bar";
    multidim[1,0] = "baz";
    multidim[1,1] = "boo";
}'
```

### Multi-dimensional iteration

```shell
zawk 'BEGIN {
    array[1,2]=3;
    array[2,3]=5;
    for (comb in array) {
        split(comb,sep,SUBSEP);
        print sep[1], sep[2],
        array[sep[1],sep[2]]
    }
}'
```

## Awk Conditions

### if-else statement

```shell
zawk -v count=2 'BEGIN {
    if (count == 1)
        print "Yes";
    else
        print "Huh?";
}'
```

#### Ternary operator

```shell
zawk -v count=2 'BEGIN {
    print (count==1) ? "Yes" : "Huh?";
}'
```

### Exists

```shell
zawk 'BEGIN {
    assoc["foo"] = "bar";
    assoc["bar"] = "baz";
    if ("foo" in assoc)
        print "Fooey!";
}'
```

#### Not exists

```shell
zawk 'BEGIN {
    assoc["foo"] = "bar";
    assoc["bar"] = "baz";
    if ("Huh" !in assoc)
        print "Huh!";
}'
```

### switch

```shell
zawk -F: '{
    switch (NR * 2 + 1) {
        case 3:
        case "11":
            print NR - 1
            break

        case /2[[:digit:]]+/:
            print NR

        default:
            print NR + 1

        case -1:
            print NR * -1
    }
}' /etc/passwd
```

## Awk Loops

### for...i

```shell
zawk 'BEGIN {
    for (i = 0; i < 10; i++)
        print "i=" i;

    for (i in 0..10)
        print f"i={i}";
}'
```

#### Powers of two between 1 and 100

```shell
zawk 'BEGIN {
    for (i = 1; i <= 100; i *= 2)
        print i
}'
```

### for...in

```shell
zawk 'BEGIN {
    assoc["key1"] = "val1"
    assoc["key2"] = "val2"
    for (key, value in assoc)
        print value;
}'
```

#### Arguments

```shell
zawk 'BEGIN {
    for (argnum in ARGV)
        print ARGV[argnum];
}' a b c
```

### Examples

#### Reverse records

```shell
zawk -F: '{ x[NR] = $0 }
    END {
        for (i = NR; i > 0; i--)
        print x[i]
    }
' /etc/passwd
```

#### Reverse fields

```shell
zawk -F: '{
    for (i = NF; i > 0; i--)
        printf("%s ",$i);
    print ""
}' /etc/passwd
```

#### Sum by record

```shell
zawk -F: '{
    s=0;
    for (i = 1; i <= NF; i++)
        s += $i;
    print s
}' /etc/passwd
```

#### Sum whole file

```shell
zawk -F: '
    {for (i = 1; i <= NF; i++)
        s += $i;
    };
    END{print s}
' /etc/passwd
```

### while

```shell
zawk 'BEGIN {
    while (a < 10) {
        print "- " " concatenation: " a
        a++;
    }
}'
```

#### do...while

```shell
zawk '{
    i = 1
    do {
        print $0
        i++
    } while (i <= 5)
}' /etc/passwd
```

### Break

```shell
zawk 'BEGIN {
    break_num = 5
    for (i = 0; i < 10; i++) {
        print i
        if (i == break_num)
            break
    }
}'
```

### Continue

```shell
zawk 'BEGIN {
    for (x = 0; x <= 10; x++) {
        if (x == 5 || x == 6)
            continue
        printf "%d ", x
    }
    print ""
}'
```

## Awk Formatted Printing

### Usage

#### Right align

```shell
zawk 'BEGIN{printf "|%10s|\n", "hello"}'

|     hello|
```

#### Left align

```shell
zawk 'BEGIN{printf "|%-10s|\n", "hello"}'

|hello     |
```

### Common specifiers

| Character     | Description            |
|---------------|------------------------|
| `c`           | ASCII character        |
| `d`           | Decimal integer        |
| `e`, `E`, `f` | Floating-point format  |
| `o`           | Unsigned octal value   |
| `x`           | Unsigned hex value     |
| `s`           | String                 |
| `%`           | Literal %              |
| `'`, `_`      | Flags: digit grouping, as in `%'d` |

### Space

```shell
zawk -F: '{
    printf "%-10s %s\n", $1, $(NF-1)
}' /etc/passwd | head -n 3
```

Outputs

```
root       /root
bin        /bin
daemon     /sbin
```

### Header

```shell
zawk -F: 'BEGIN {
    printf "%-10s %s\n", "User", "Home"
    printf "%-10s %s\n", "----","----"}
    { printf "%-10s %s\n", $1, $(NF-1) }
' /etc/passwd | head -n 5
```

Outputs

```
User       Home
----       ----
root       /root
bin        /bin
daemon     /sbin
```

### f-string format

| Placeholder  | Value     | Result      |
|--------------|-----------|-------------|
| `{p:.2f}`    | `3.14159` | `3.14`      |
| `{n:5d}`     | `42`      | `   42`     |
| `{n:<5}`     | `42`      | `42   `     |
| `{n:05}`     | `42`      | `00042`     |
| `{s:*^6}`    | `ab`      | `**ab**`    |
| `{n:x}`      | `42`      | `2a`        |
| `{r:.1%}`    | `0.256`   | `25.6%`     |
| `{s:.2}`     | `hello`   | `he`        |
| `{n:,}`      | `1234567` | `1,234,567` |
| `{n:_d}`     | `1234567` | `1_234_567` |
| `{p:,.2f}`   | `1234.5`  | `1,234.50`  |

```shell
zawk 'BEGIN {price = 3.14159; name = "zawk"; print f"{NR+1}: {price:.2f} {name:>10}"}'

1: 3.14       zawk
```

## zawk Syntax

### At a glance

| Syntax              | Example                                |
|---------------------|----------------------------------------|
| f-string            | `f"user {$1} at line {NR:05d}"`        |
| Raw string          | `r"C:\Program Files"`, `r#"say "hi""#` |
| Default value       | `"Hello " name ?? "Guest"`             |
| Default assignment  | `OFS ??= ","`                          |
| `try ... else`      | `body = try read_all(url) else ""`     |
| Not in array        | `$1 !in seen { seen[$1]; print }`      |
| Local variables     | `local total = 0, seen[]`              |
| Key-value loop      | `for (k, v in count) print k, v`       |
| Range loop          | `for (i in 1..=10)`                    |
| Negative field      | `$(-1)`, `$(-2)`                       |
| Field list          | `zawk '$1,$3' file`                    |

See: [New AWK Syntax](./syntax/overview.md)

### match expression

```shell
echo "GET /api took 1532ms" | zawk '{
    print match $0 {
        /(\d+)ms/ as m => m[1] / 1000 " s",
        "error" | "fatal" => "failed",
        _ => "-"
    }
}'

1.532 s
```

### try...else

```shell
zawk 'BEGIN {
    body = try read_all("/no/such/file") else "default"
    print body   # => default
    print ERRNO  # => read_all: /no/such/file: No such file ...
}'
```

An HTTP 404 is a valid response for `http_get`: check `r["status"]`.

### Local variables

```shell
echo "1 2 2" | zawk '{
    local total = 0, seen[]
    for (i = 1; i <= NF; i++) {
        if ($i in seen) continue
        seen[$i]
        total += $i
    }
    print total  # => 3
}'
```

### Ranges

```shell
# 1..4 excludes the end, 1..=4 includes it
zawk 'BEGIN {
    for (i in 1..4)  printf "%d ", i   # => 1 2 3
    print ""
    for (i in 1..=4) printf "%d ", i   # => 1 2 3 4
    print ""
}'
```

### Sorted iteration

```shell
zawk 'BEGIN {
    count["a"] = 3; count["b"] = 9; count["c"] = 1
    PROCINFO["sorted_in"] = "@val_num_desc"
    for (word, n in count) print n, word
}'
```

### assert

```shell
# Print the message to stderr and exit with code 2, END is not run
zawk -F: '{ assert(NF > 1, f"line {NR}: bad record") }' /etc/passwd
```

### Command-line sugar

```shell
zawk '$1,$NF' /etc/passwd    # zawk '{ print $1, $NF }'
zawk '/root/' /etc/passwd    # Print matching lines
```

Use `--posix` to disable the sugar.

## Structured Data

### Input formats

| Option                    | Format                                 |
|---------------------------|----------------------------------------|
| `-i csv`                  | CSV, add `-H` for a header line        |
| `-i tsv`                  | TSV                                    |
| `-i jsonl`                | JSON Lines, implies `-H`               |
| `-i json`                 | JSON array or document                 |
| `-i parquet`              | Parquet, implies `-H`                  |
| `-i markdown`             | First Markdown table, implies `-H`     |
| `-i xlsx`                 | Excel or ODS sheet                     |

```shell
zawk -H -i csv '$FI["age"] > 40 {print $FI["name"]}' demo.csv
zawk -i jsonl '$FI["age"] > 40 {print $FI["name"]}' demo.jsonl
zawk -i json '{print $FI["name"]}' demo-array.json
zawk -H -i xlsx '{print $1}' demo.xlsx
```

Select a part of a document with `data.json#$.items` or `book.xlsx#Sheet2`.

### Input sources

| Source     | Example                                                  |
|------------|----------------------------------------------------------|
| Local      | `tests/demo.csv`                                         |
| Pipe       | `ls -al \| zawk '{print $1}'`                            |
| HTTP(S)    | `http://example.com/path/to/file`                        |
| S3         | `s3://bucket-name/path/to/file`                          |
| Compressed | `access.log.gz` (gz, zst, bz2, xz, lz4)                  |
| SQLite     | `'sqlite://./app.db#user'`                               |
| MySQL      | `'mysql://root:123456@127.0.0.1:3306/test#people'`       |
| PostgreSQL | `'postgresql://u:p@host:5432/db#select name from people'` |
| NATS       | `nats://127.0.0.1:4222/topic1`                           |
| Kafka      | `kafka://127.0.0.1:9092/topic1`                          |

### Output formats

The first record printed is the header, except for `-o csv` and `-o tsv`.

| Option        | Format                                    |
|---------------|-------------------------------------------|
| `-o csv`      | CSV, correctly quoted                     |
| `-o tsv`      | TSV                                       |
| `-o jsonl`    | JSON Lines, `age:number` for typed values |
| `-o markdown` | Markdown table                            |
| `-o table`    | Aligned table for terminals               |
| `-o parquet`  | Parquet, with `--out-file`                |

```shell
zawk -H -i csv -o csv '{print $2, $3}' demo.csv
zawk -H -i csv -o table 'NR==1 {print "name", "age"} {print $2, $3}' demo.csv
zawk -H -i csv -o jsonl 'NR==1 {print "name", "age:number"} {print $2, $3}' demo.csv
zawk -H -i csv -o parquet --out-file out.parquet \
    'NR==1 {print "id:int64", "name"} {print $1, $2}' demo.csv
```

### Parallel execution

```shell
# -p r: within a file, -p f: between files
zawk -H -i csv -p r -j 8 '{sum += $3} END {print sum}' demo.csv
```

## Standard Library

### Text

| Function                                  | Description                       |
|-------------------------------------------|-----------------------------------|
| `trim(s)`, `trim("[a]", "[]")`            | Remove whitespace or given chars  |
| `truncate(s, 8)`                          | `hello...`                        |
| `pad(s, 5)`, `pad_start(s, 3, "0")`, `pad_end(s, 5)` | Pad to a length        |
| `starts_with(s, t)`, `ends_with(s, t)`, `contains(s, t)` | Test a substring   |
| `camel_case(s)`, `snake_case(s)`, `kebab_case(s)`, `title_case(s)` | Change case |
| `repeat("*", 3)`                          | `***`                             |
| `char_at(s, 2)`, `last_index(s, t)`       | Character, last position          |
| `mask("abc@example.com")`                 | `abc*********com`                 |
| `is("email", s)`, `is("ipv4", s)`         | Validate a format                 |
| `isint(x)`, `isnum(x)`                    | Test a number                     |
| `similar("hello", "hallo")`               | Similarity, `0.88`                |
| `format_bytes(1024)`, `to_bytes("2 KB")`  | `1 KB`, `2048`                    |
| `escape("json", s)`                       | Also csv, tsv, xml, html, sql, shell |

See: [Text functions](./stdlib/text.md)

### Parsers

| Function                               | Result           |
|----------------------------------------|------------------|
| `url("https://a.com:8080/x")["port"]`  | `8080`           |
| `path("/tmp/a.txt")["file_ext"]`       | `txt`            |
| `semver("1.2.3-alpha")["minor"]`       | `2`              |
| `pairs("id=1,name=x")["name"]`         | `x`              |
| `parse("Hello World", "{a} {b}")["b"]` | `World`          |
| `rparse("Hello World", "(\\w+) (\\w+)")[2]` | `World`     |

See: [Parsers](./stdlib/parsers.md)

### JSON, XML, HTML and CSV

| Function                          | Description                 |
|-----------------------------------|-----------------------------|
| `json_value(s, "$.user.name")`    | Value at a JSONPath         |
| `json_query(s, "$.tags[*]")`      | Array of values             |
| `json_set(s, "id", "2")`          | Update a key                |
| `from_json(s)`, `to_json(arr)`    | JSON to array, and back     |
| `xml_value(s, "/a/b")`            | Value at an XPath           |
| `html_value(s, "p.x")`            | Text of a CSS selector      |
| `from_csv(s)`, `to_csv(arr)`      | CSV row to array, and back  |

See: [JSON / XML / HTML / CSV](./stdlib/json-xml-html.md)

### Array and Math

| Function                                  | Description                  |
|-------------------------------------------|------------------------------|
| `_sum(a)`, `_mean(a)`, `_median(a)`       | Aggregate values             |
| `_max(a)`, `_min(a)`, `_stddev(a)`        | Aggregate values             |
| `_quantile(a, 0.5)`                       | q-quantile                   |
| `_count_distinct(a)`, `_top(a, 2)`        | Distinct count, top keys     |
| `_join(a, "-")`                           | Join values                  |
| `seq(1, 3)`, `uniq(a)`                    | Sequence, unique values      |
| `asort(a)`, `asorti(a)`                   | Sort values, sort indices    |
| `abs(x)`, `ceil(x)`, `floor(x)`, `round(x)` | Rounding                   |
| `min(1, 2)`, `max("A", "B")`              | Smallest, largest            |
| `eval("1+2*3")`, `fend("1 km to m")`      | `7`, `1000 m`                |

See: [Array & Math](./stdlib/array-math.md)

### Date and Time

| Function                           | Description               |
|------------------------------------|---------------------------|
| `systime()`                        | Current timestamp         |
| `strftime("%Y-%m-%d", ts)`         | Format a timestamp        |
| `mktime("2026-10-31 12:00:00")`    | Parse to a timestamp      |
| `datetime(ts)["year"]`             | Date parts                |
| `duration("1h30m")`                | `5400` seconds            |
| `date_add("2026-10-31", 1, "day")` | Add to a date             |
| `date_diff("day", start, end)`     | Difference between dates  |
| `date_trunc("day", ts)`            | Truncate a timestamp      |
| `to_iso8601(0)`                    | `1970-01-01T00:00:00.000Z` |

See: [Date/Time](./stdlib/datetime.md)

### Crypto and Encoding

| Function                                | Description            |
|-----------------------------------------|------------------------|
| `digest("sha256", s)`                   | Hash, alias `hash`     |
| `hmac("sha256", key, s)`                | HMAC                   |
| `encode("base64", s)`, `decode("base64", s)` | Also hex, url etc. |
| `encrypt("aes-256-gcm", s, key)`, `decrypt(...)` | Encrypt, decrypt |
| `jwt("HS256", key, payload)`, `dejwt(key, token)` | JSON Web Token |
| `uuid()`, `uuid("v7")`, `ulid()`, `tsid()`, `snowflake()` | IDs |

See: [Crypto & Encoding](./stdlib/crypto-encoding.md)

### Network

```awk
r = http_get("http://127.0.0.1:18080/get")
print r["status"], r["text"]

headers["Content-Type"] = "text/plain"
r = http_post("http://127.0.0.1:18080/post", "hello", headers)

send_mail("bot@example.com", "ops@example.com", "Report", body)
publish("nats://127.0.0.1:4222/orders", to_json(order))
publish("kafka://127.0.0.1:9092/orders", to_json(order))
publish("mqtt://127.0.0.1:1883/sensors/temp", $2)
text = s3_get("bucket1", "reports/2026-10.csv")
s3_put("bucket1", "reports/summary.json", to_json(summary))
```

See: [Network](./stdlib/network.md)

### Database

```awk
params[1] = 3
rows = sqlite_query("app.db", "select name from people where id = ?", params)
print rows[1]

url = "mysql://root:123456@127.0.0.1:3306/test"
rows = mysql_query(url, "select name from people where id = ?", params)
n = mysql_execute(url, "delete from people where id = ?", params)

url = "postgres://postgres:123456@127.0.0.1:15432/demo_db"
rows = pg_query(url, "select name from people where id = $1", params)
```

See: [Database & KV](./stdlib/database-kv.md)

### KV

```awk
kv_put("namespace1", "key", "value")    # SQLite at $HOME/.awk/sqlite.db
print kv_get("namespace1", "key")
kv_delete("namespace1", "key")
kv_clear("namespace1")

kv_put("redis://127.0.0.1:6379/jobs", "offset", NR)
kv_put("nats://127.0.0.1:4222/bucket1", "offset", NR)
```

### OS and I/O

| Function                               | Description                    |
|----------------------------------------|--------------------------------|
| `getenv("HOME", "/tmp")`               | Environment with a default     |
| `whoami()`, `os()`, `arch()`, `pwd()`  | System information             |
| `system2("ls")`                        | `code` and `stdout` of a command |
| `read_all(path)`, `write_all(path, s)` | Read and write a whole file    |
| `read_config("demo.ini")`              | Read an INI or properties file |
| `typeof(x)`, `isarray(x)`              | Type of a value                |
| `var_dump(x)`, `pprint(x)`             | Print a value to stderr        |
| `log_info(msg)`                        | Also `log_debug`, `log_warn`, `log_error` |
| `fake("email")`                        | Fake data                      |

See: [OS, I/O & Misc](./stdlib/os-io.md)

## Command Line

### Commands

| Command                               | Description                       |
|---------------------------------------|-----------------------------------|
| `zawk init demo.awk`                  | Create an AWK file with help info |
| `zawk functions trim`                 | List builtin functions            |
| `zawk fmt demo.awk`                   | Format a program                  |
| `zawk lint demo.awk`                  | Check a program, report as JSON   |
| `zawk lint -H -i csv -f demo.awk`     | Lint for CSV                      |
| `zawk lint -i parquet -f demo.awk`    | Lint for Parquet                  |
| `zawk inspect demo.csv`               | Schema, rows and statistics       |
| `zawk inspect demo.parquet`           | Inspect Parquet                   |
| `zawk dump demo.txt`                  | Dump text to CSV                  |
| `zawk llm -H -i csv demo.csv 'how many adults?'` | Print a prompt for an LLM |

### Options

| Option                   | Description                                        |
|--------------------------|----------------------------------------------------|
| `-f file`, `-e 'code'`   | Program from a file or text                        |
| `-E file`                | Same as `-f`, for `#!` scripts                     |
| `-F fs`                  | Field separator                                    |
| `-v var=val`             | Assign a variable                                  |
| `-H`                     | Parse the header line into `FI`                    |
| `-i`, `-o`               | Input and output format                            |
| `--out-file file`        | Write to a file                                    |
| `--summary`              | Column statistics, with `-o table` or `-o markdown` |
| `--null text`            | Read NULLs of database, JSON Lines and Parquet as text |
| `--include lib`          | Load an AWK library, same as `@include "lib"`      |
| `-p r`, `-p f`, `-j n`   | Parallel by record or file, with n workers         |
| `--lint`                 | Lint before running                                |
| `--stats`                | Print run metrics as JSON to stderr                |
| `--posix`                | Disable command-line sugar                         |

## Miscellaneous

### Regex Metacharacters

`\` `^` `$` `.` `[` `]` `|` `(` `)` `*` `+` `?`

### Escape Sequences

| -    | -                   |
|------|---------------------|
| `\b` | Backspace           |
| `\f` | Form feed           |
| `\n` | Newline (line feed) |
| `\r` | Carriage return     |
| `\t` | Horizontal tab      |
| `\v` | Vertical tab        |

### Run script

```shell
$ cat demo.awk
#!/usr/bin/env -S zawk -E
BEGIN { x = 23 }
      { x += 2 }
END   { print x }
$ seq 3 | zawk -f demo.awk
29
```

## Also see

- [zawk overview](https://github.com/linux-china/zawk/blob/master/info/overview.md)
- [zawk vs gawk](./zawk-vs-gawk.md)
- [Differences from gawk](./compatibility.md)
- [Snippets](./snippets.md)
