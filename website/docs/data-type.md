---
sidebar_position: 2
title: Data Types
sidebar_label: Data Types
---

# Data Types

A zawk program works with three kinds of values:

| Type          | What it is                                                                         | `typeof(x)`                                        |
|---------------|------------------------------------------------------------------------------------|----------------------------------------------------|
| **scalar**    | a number or a string, converted into each other by AWK's rules                     | `"number"`, `"string"` |
| **array**     | an associative array from scalar keys to scalar values                             | `"array"`                                          |
| **dataframe** | an in-memory table with named, typed columns, built and queried with `df_*`        | `"dataframe"`                                      |

```awk
BEGIN {
    n = 42; s = "hello"                 # scalars: number, string
    a["x"] = 1; a["y"] = 2              # array
    d = df_new("name,age:int")          # dataframe
    print typeof(n), typeof(s), typeof(a), typeof(d), typeof(u)
    # number string array dataframe untyped
}
```

No declarations are needed. zawk infers a static type for every variable (integer, float or string; arrays
specialized by key and value type) and compiles the program to machine code, while keeping AWK's dynamic
feel. See [The Role of Types in zawk](https://github.com/linux-china/zawk/blob/master/info/types.md) for how
the inference works.

## Scalars: number and string

A scalar is either a number or a string, and AWK converts between the two on demand:

- Arithmetic (`+ - * / % ^`) uses the numeric value: `"3" + 2` is `5`, `"12abc" + 0` is `12`, `"abc" + 0` is `0`.
- Concatenation uses the string value: `1 2` is `"12"`. A number is turned into a string with `CONVFMT`
  (`OFMT` when printed); integral values print without decimals: `0.1 + 0.2` prints `0.3`, `10 / 2` prints `5`.
- An unassigned variable is both `""` and `0`.

```awk
BEGIN {
    x = "0"; y = x + 2; x = y + 1
    print x, y              # 3 2
    print "3" + 2, 1 2      # 5 12
    print u + 1, "[" u "]"  # 1 []
}
```

### Numeric literals

Numbers in a program can be written in decimal (`42`, `4.2e1`), hexadecimal (`0x2A`), octal (`0o52`) or binary
(`0b101010`). Prefixes may be upper case and `_` may group digits: `0b1010_1010`, `1_000_000`. A literal is an
integer when it is exact as a double (up to 2^53), a float otherwise. Hexadecimal, octal and binary literals have
no fraction: `0o7.5` and `0x1.8` are syntax errors, not `0o7` concatenated with `.5`.

```awk
BEGIN { print 0x2A, 0o52, 0b101010, 1_000_000 }   # 42 42 42 1000000
```

Converting a *string* to a number (`"0x2A" + 0`, `float(s)`, a comparison) only reads decimal and `0x`
hexadecimal. To parse octal and binary strings, use [`strtonum`](#how-strtonum-works).

### strnum: strings from input

A string that comes from input — a field `$1`, `getline`, `split`, `ARGV`, `ENVIRON`, a `-v` assignment — is a
**strnum**: when it looks like a number, it compares numerically; otherwise it compares as a string. String
constants and computed strings (concatenation, `substr`, ...) always compare as strings.

```shell
$ echo "10.0 abc" | zawk '{ print typeof($1), typeof($2), ($1 == 10), ("10.0" == 10) }'
strnum strnum 1 0
```

`$1 == 10` is true because the field `10.0` looks like a number, while the constant `"10.0"` is compared with
`"10"` as strings. To force a comparison, write `$1 + 0` (numeric) or `$1 ""` (string).

## Arrays

An array maps scalar keys to scalar values. Keys are strings (integer keys are stored efficiently), elements
are created on first use, and `a[i, j]` joins the subscripts with `SUBSEP`.

```awk
BEGIN {
    a["apple"] = 3; a["pear"] = 5; m[1, 2] = "x"
    for (k, v in a) print k, v          # zawk: key and value
    print ("apple" in a), ("kiwi" !in a), length(a)   # 1 1 2
    delete a["pear"]
}
```

- An array is not a scalar: it cannot be assigned to a variable, returned from a function or used as a
  condition. It is passed to functions by reference.
- Arrays of arrays (`a[i][j]`) are not supported: use `a[i, j]`.
- Many stdlib functions take or fill arrays: `split`, `asort`, `_sum`, `_median`, `json_decode`, `df_col`, ...

## DataFrames

A dataframe is an in-memory table with named, typed columns (`int64`, `double`, `bool`, `string`), handy in
`END` for grouping, joining, sorting and pivoting. It adds **no new type to the language**: its value is a
string handle such as `"df#1"`, so it can be stored in variables and arrays, passed to and returned from
functions, and printed. `typeof` returns `"dataframe"` while the table exists and `"string"` after `df_free(d)`.

```awk
BEGIN {
    d = df_new("name,age:int")
    df_append(d, "Jackie", 48); df_append(d, "Leo", 24); df_append(d, "Lucas", 10)
    adults = df_filter(d, "age >= 18")
    print d, typeof(d), df_nrow(adults)      # df#1 dataframe 2
    print df_get(adults, 2, "name")          # Leo
    df_print(df_sort(d, "age"), "table")
}
```

Values move between dataframes and scalars/arrays with `df_get`, `df_row`, `df_col`, `df_to_map` and
`df_append`, `df_from_array`, following the scalar rules. See [DataFrames](stdlib/dataframe.md).

## How strtonum works

`strtonum(s)` converts a string to a number, like gawk's, with zawk extensions for octal, binary and thousands
separators. The prefix of the string chooses the base, and parsing stops at the first character that does not
belong:

| Input                       | Read as                                       | Example                                   |
|-----------------------------|-----------------------------------------------|-------------------------------------------|
| `0x` / `0X`                 | hexadecimal                                   | `strtonum("0x1F")` → 31                   |
| `0o` / `0O` (zawk)          | octal                                         | `strtonum("0o52")` → 42                   |
| `0b` / `0B` (zawk)          | binary                                        | `strtonum("0b101010")` → 42               |
| a leading `0`               | octal, unless `8`, `9`, `.` or `e` follows    | `strtonum("017")` → 15, `"018"` → 18      |
| anything else               | decimal                                       | `strtonum("12abc")` → 12, `"abc"` → 0     |
| `,` or `_` groups (zawk)    | decimal with thousands separators             | `strtonum("1,234.5")` → 1234.5            |

Details:

- **Invalid digits stop parsing**: `strtonum("0b12")` is 1, `strtonum("0o78")` is 7, `strtonum("0x")` is 0.
- **A sign or blanks before a prefix make it decimal**, as in gawk: `strtonum("-0x10")`, `strtonum(" 0x10")` and
  `strtonum("-0b1")` are 0, and `strtonum(" 017")` is 17. (Ordinary conversion differs: `"-0x10" + 0` is -16.)
- **Thousands separators** use one kind per number; the first group has 1-3 digits and every later group
  exactly 3: `strtonum("1_000")` is 1000, but `strtonum("1,2")` is 1 and `strtonum("1,234_567")` is 1.
  Only `strtonum` reads them: `"1_000" + 0` is 1.
- **A strnum that looks like a number keeps its value**: for a field `017`, `strtonum($1)` is 17 (not 15), and
  for a field `-0x10` it is -16. To parse the text with the prefix rules, make it a plain string first:
  `strtonum(substr($1, 1))` is 15 for `017`.

```awk
BEGIN {
    print strtonum("0x1F"), strtonum("0o17"), strtonum("0b1010"), strtonum("017")   # 31 15 10 15
    print strtonum("018"), strtonum("-0x10"), strtonum("1,234,567")                 # 18 0 1234567
}
```

```shell
$ echo "017 0b101 1,250" | zawk '{ print strtonum($1), strtonum($2), strtonum($3) }'
17 5 1250
```

## True and false in conditions

`if`, `while`, `for`, `?:`, `!`, `&&`, `||` and patterns test a value as follows:

| Value                                  | False                                          | True                                  |
|----------------------------------------|------------------------------------------------|---------------------------------------|
| number                                 | `0`, `0.0`, `-0`                               | any other number, including `NaN`     |
| unassigned variable / missing element  | always                                         | —                                     |
| string constant or variable            | `""`, and a string that looks like zero: `"0"`, `"0.0"`, `" +0 "`, `"0x0"` | any other string: `"abc"`, `" "`, `"00a"`, `"false"` |
| strnum (field, `getline`, `split`)     | empty, or looks like zero (`0`, `0.00`, `-0`)  | anything else                         |
| regex literal `/re/`                   | `$0` does not match                            | `$0` matches                          |
| array                                  | not allowed: test `length(a)` or `k in a`      |                                       |
| dataframe handle                       | never (it is a non-empty string)               | always: test `df_nrow(d)` instead     |

The string rule is also the one of `??` and `mkbool`, so `x ? x : d` and `x ?? d` agree.

```awk
BEGIN {
    print !0, !1, !"", !"abc", !"0", !"0.0", !" "    # 1 0 1 0 1 1 0
    if (!u) print "unassigned is false"
    print ("false" ? "T" : "F")                      # T: a non-empty word
    print (mkbool("false") ? "T" : "F")              # F: mkbool reads true/false, yes/no, on/off
    d = df_new("a:int")
    print (d ? "T" : "F"), (df_nrow(d) ? "T" : "F")  # T F
}
```

```shell
$ printf '0\n0.0\n\nabc\n3\n' | zawk '$1 { print NR": "$1 }'
4: abc
5: 3
```

:::note gawk difference
In gawk a non-empty string **constant** such as `"0"` or `"0.0"` is true. zawk treats a string that looks like
zero as false, and `zawk lint` warns about string constants used as conditions. Compare explicitly when it
matters: `x != ""`, `x + 0 != 0`, `x == "0"`.
:::
