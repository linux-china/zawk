# AWK compatibility tests

Each `cases/*.awk` program is run by `tests/compat.rs` (`cargo test --test compat`) with both
backends (`-Binterp`, `-Bcranelift`), and its stdout and exit code are compared with
`cases/*.out`, the output of gawk. The test does not need gawk installed.

The cases are written for zawk, organized like the POSIX specification and the onetrue-awk
`T.*` tests (gawk's own test suite is GPL licensed, so it is not copied here):

| Prefix | Area |
|---|---|
| `01-` | records and fields: `$n`, `NF`, `FS`, `RS`, `OFS`/`ORS`, `NR`/`FNR`, operands |
| `02-` | patterns: regex, expressions, ranges, `BEGIN`/`END`, dynamic regex |
| `03-` | expressions: arithmetic, precedence, numeric strings, number output |
| `04-` | string functions: `length`, `substr`, `index`, `match`, `split`, `sub`/`gsub`, UTF-8 |
| `05-` | `printf` formats |
| `06-` | arrays: `in`, `delete`, `SUBSEP`, `for (k in a)` |
| `07-` | control flow: loops, `next`, `exit` |
| `08-` | user defined functions |
| `09-` | `getline`, redirection, `system()`, `ARGV`/`ENVIRON` |
| `10-` | classic one-liners from "The AWK Programming Language" |
| `11-` | gawk extensions |

## Writing a case

A case is an AWK program file; directives in its leading comments describe how it runs
(paths are relative to this directory):

```awk
# stdin: data/emp.data
# opts: -F\t -v min=10
# args: data/a.txt data/b.txt
# exit: 3
$3 > min { print $1 }
```

| Directive | Meaning | Default |
|---|---|---|
| `stdin` | file used as standard input | empty input |
| `opts` | options before `-f` | none |
| `args` | operands after the program: files, `var=value` | none |
| `exit` | expected exit code | `0` |

Keep the output deterministic: do not depend on the order of `for (k in a)`, time, or randomness.

Then generate the expected output with gawk and run the test:

```sh
tests/compat/regen.sh NAME     # or no argument to regenerate all cases
cargo test --test compat
```

## Known failures

`known_failures.txt` lists the cases where zawk differs from gawk today, with the reason. They are
still run, and the test fails when one of them passes, so remove the line after fixing the
difference. A new case that fails must either be fixed in zawk or added to the list.

## CI

The `awk-compatibility` job in `.github/workflows/build.yml` installs gawk, runs
`tests/compat/regen.sh --check` to verify that the committed `*.out` files still match gawk, and
then runs `cargo test --test compat`.
