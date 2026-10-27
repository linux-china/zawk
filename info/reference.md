# zawk Builtin Functions and Commands

This document lists all of the builtin functions and commands supported by
zawk. For those interested in a source of truth on these components, check out
the "builtins" module in
[`src/builtins.rs`](https://github.com/linux-china/zawk/blob/master/src/builtins.rs).

Unlike Awk, builtin functions must have parentheses directly following the
function name. Awk supports C-style syntax like `length (s)`, but only with
builtin functions: user-defined functions must still be called like `foo(x)`. In
zawk, builtin and user-defined functions are called with the same syntax: with
no spaces allowed.

## Operators

_Binary operators:_
* Arithmetic: `+`, `-`, `/`, `*`, `*`, `^` (which is exponentiation), and `%`
* Comparison (which also work on strings): `<`, `>`, `<=`, `>=`, `==`, `!=`.
* Membership: `k in arr` is 1 when `arr` has the key `k`; `k !in arr` is the same as `!(k in arr)`, e.g.
  `$1 !in seen { seen[$1]; print }`. Multi-dimensional keys work with both: `(i, j) in arr`, `(i, j) !in arr`.
* Default value: `l ?? r` is `r` when `l` is "empty", and `l` otherwise. `l` is evaluated once.
  * Empty means uninitialized, `""`, or a number that is zero. A string counts as a number when it
    looks like one (as a strnum does), so `"0"`, `"0.0"`, `"+0"` and `" -0 "` are empty too, even as
    string constants (unlike `if ("0.0")`, which is false). A field holding `0` is therefore replaced:
    for a numeric column where `0` is valid data, write `($3 == "" ? 0 : $3)` instead.
  * An array element on the left is only read when it exists, as with `k in a`: `a[k] ?? "none"`
    does not create `a[k]`.
  * `??` binds tighter than any other operator except `++`/`--`, and is left associative:
    `"Hello " name ?? "Guest"` is `"Hello " (name ?? "Guest")`, `-u ?? 5` is `-(u ?? 5)`,
    `1 + u ?? 5` is `1 + (u ?? 5)`, `i++ ?? 0` is `(i++) ?? 0` and `x ?? y ?? "z"` is `(x ?? y) ?? "z"`.
  * The right operand is a term, optionally with `++`/`--` or preceded by unary `-`, `+` or `!`
    (`u ?? -1`, `u ?? x++`, `u ?? !v`). Anything else needs parentheses: `u ?? (a + 1)`. A following
    `^` applies to the whole `??`: `u ?? 2 ^ 2` is `(u ?? 2) ^ 2`.
* Default assignment: `l ??= r` is `l ?? (l = r)`, like `??=` in JavaScript and C#: when `l` is empty (as for
  `??`), `r` is evaluated and assigned to `l`; otherwise `r` is not evaluated and `l` is unchanged. Its value is
  the new value of `l`. It has the precedence of `=` (`r` can be any expression), and the subscript of an array
  element is evaluated once: `{ sep ??= ","; total[$1] ??= 0 }`, `seen[i++] ??= NR`.
* Error handling: `try E else F` clears `ERRNO` and evaluates `E`. If nothing failed in `E` (no stdlib function,
  `getline` or `close` set `ERRNO`), the result is `E` and `ERRNO` gets back its value from before the `try`;
  otherwise the result is `F`, which can read `ERRNO` (and `ERRNO` keeps the error). Stdlib functions failing
  inside `E` don't print warnings.
  ```awk
  body = try http_get(url)["text"] else ""
  id   = try json_value(s, "$.id") else error_handler(ERRNO)
  try save(rec) else log_error(ERRNO)        # as a statement
  ```
  * `E` and `F` must both be scalars or both be arrays. `http_get` returns an array (`r["status"]`, `r["text"]`),
    so a scalar fallback is an error:
    ```awk
    r    = try http_get(url) else ""            # error: attempt to use an array in a scalar context
    body = try http_get(url)["text"] else ""    # scalar: E is the "text" element
    r    = try http_get(url) else NONE          # array: NONE is never assigned, so it is an empty array
    if (ERRNO != "") print "failed: " ERRNO
    ```
    A variable that is never assigned is an empty array of the type of `E`. As arrays are passed by reference,
    `r` is then the array `NONE` itself: writing `r["k"]` also changes `NONE`. In a function, declare a fresh
    empty array instead: `local empty[]; return try http_get(url) else empty`. A parse function such as
    `from_json("")` also gives an empty array, but of a fixed type (`map[str]str`), which may not be the type of `E`.
  * `else` is required, and `try` is a keyword (it can't name a variable or function).
  * `E` is a conditional expression and `F` extends to the end of an assignment: `x = try f() else g() + 1`
    falls back to `g() + 1`. A newline may follow `else`. As the operand of a concatenation or another
    operator, `try` needs parentheses: `"id: " (try f() else "none")`.
  * In `print`, `>` after `F` is still a redirection: `print try f() else "x" > "out.txt"`. For the same reason,
    `E` can't hold an unparenthesized `>` there: write `print (try a > b else c)`.
  * A function without `return` gives an uninitialized value, so `try g(x) else h(ERRNO)` works as a statement.
  * A failure handled by an inner `try` doesn't count for an outer one: with
    `function safe() { return try a() else "default" }`, `try safe() else "x"` is `"default"`, and in
    `try (try a() else b()) else c()`, `c()` runs only if `b()` fails.
  * `try` is not exception handling: fatal errors (division by zero, a result above the length limit of
    `repeat`/`pad`/`seq`, an array used as a scalar, ...) still end the program.

_Unary Operators:_

* `$x`: Get column `x`. A negative index counts from the end: `$(-k)` is `$(NF + 1 - k)`, so `$(-1)` is
  `$NF` and `$(-2)` is `$(NF-1)`. Reading before the first field (`k > NF`) gives `""`, assigning there is a
  fatal error. The parentheses are required: `$-1` is a syntax error. Only a literal `k` counts from the end:
  a negative index computed at runtime (`$(NF-2)` on a one-field record, `$(-k)` with a variable `k`) is a
  fatal error, as in gawk.
* `+`, `-`: Unary "positive" and negation.
* `!`: logical negation.

## Statements

* `for (k in arr) body`: iterate over the keys of `arr` (order set by `PROCINFO["sorted_in"]`, unspecified
  otherwise). `arr` may be a function call returning an array.
* `for (k, v in arr) body`: key-value iteration, the same as `for (k in arr) { v = arr[k]; body }`, e.g.
  `END { for (k, v in cnt) print k, v }`. On an array made by `split` it works like Python's `enumerate`:
  `for (i, x in a)`. Unlike `for (k in arr)`, `arr` must be an array variable: `for (k, v in f())` is a syntax
  error, so assign the result first (`a = f(); for (k, v in a) ...`).

## Math

* Floating-point operations: `sin`, `cos`, `atan`, `atan2`, `log`, `log2`,
  `log10`, `sqrt`, `exp` are delegated to the Rust standard library, or LLVM
  intrinsics where available.
* `rand()`: Returns a uniform random floating-point number between 0 and 1.
* `srand(x)`: Seeds the random number generator used by `rand`, returns the old
  seed.
* Bitwise operations. All of these operations coerce their operands to integers
  before being evaluated.
  * `compl(x)`: Bitwise complement.
  * `and(x, y)`: Bitwise and.
  * `or(x, y)`: Bitwise or.
  * `xor(x, y)`: Bitwise xor.
  * `lshift(x, y)`: Shift `x` left by `y` bits.
  * `rshift(x, y)`: Arithmetic right shift of `x` by `y` bits.
  * `rshiftl(x, y)`: Logical right shift of `x` by `y` bits.

## String Operations

* `s ~ re`: 1 if string `s` matches regular expression in `re`.
* `s !~ re`: Equivalent to negating the result of `s ~ re`.
* `match(s, re)`: 1 if string `s` matches the regular expression in `re`. If `s`
  matches, the `RSTART` variable is set with the start of the leftmost match of
  `re`, and `RLENGTH` is set with the length of this match.
* `substr(s, i[, j])`: The 1-indexed substring of string `s` starting from index `i`
  and continuing for the next `j` characters or until the end of `s` if `i+j`
  exceeds the length of `s` or if `s` is not provided.
* `sub(re, t, s)`: Substitutes `t` for the first matching occurrence of regular
  expression `re` in the string `s`.
* `gsub(re, t, s)`: Like `sub`, but with all occurrences substituted, not just
  the first.
* `index(haystack, needle)`: The first index within `haystack` in which the
  string `needle` occurs, 0 if `needle` does not appear.
* `split(s, m[, fs])`: Splits the string `s` according to `fs`, placing the
  results in the array `m`. If `fs` is not specified then the `FS` variable is
  used to split `s`.
* `sprintf(fmt, s, ...)`: Returns a string formatted according to `fmt` and
  provided arguments. The goal is to provide the semantics of the libc `sprintf`
  function.
* `print(s, ...) [>[>] out]`: Print the arguments `s` separated by `OFS`. If `>>
  out` is provided then the output is appended to the file `out`, if `> out` is
  provided then any data in `out` is overwritten. Parentheses are optional in
  `print`, but parsing of non-parenthesized arguments proceeds differently to
  avoid potential ambiguities.
* `printf(fmt, s, ...) [>[>] out]`: Like `sprintf` but the result of the
  operation is written to standard output, or to `out` according to the append
  or overwrite semantics specified by `>` or `>>`. Like `print`, `printf` can be
  called without parentheses around its arguments, though arguments are parsed
  differently in this mode to avoid ambiguities.
* `hex(s)`: Returns the hexadecimal integer (e.g. `0x123abc` or `ff`, after
  optional blanks and sign) encoded in `s`, or `0` otherwise. The digits are read
  as a 64-bit unsigned value with the same bits as the result, so
  `hex("ffffffffffffffff")` is `-1`.
* `join_fields(i, j[, sep])`: Returns columns `i` through `j` (1-indexed,
  inclusive) concatenated together, joined by `sep`, or by `OFS` if `sep` is not
  provided.
* `escape_csv(s)`: Returns `s` escaped as a CSV column per RFC 4180: adds quotes
  if `s` contains `"`, `,`, a line break or leading/trailing spaces, and doubles
  quotes. Line breaks are kept as is inside the quotes.
* `escape_tsv(s)`: Returns `s` escaped as a TSV column. There is less to do with
  CSV, but tab and newline characters are replaced with `\t` and `\n`.
* `join_csv(i, j)`: Like `join_fields` but with columns joined by `,` and
  escaped using `escape_csv`.
* `join_tsv(i, j)`: Like `join_fields` but with columns joined by tabs and
  escaped using `escape_tsv`.
* `int(s)`: Convert `s` to an integer. Floating-point numbers are also converted
  (rounded down), potentially without a round-trip through a string
  representation.
* `tolower(s)`: Returns a copy of `s` where all uppercase ASCII characters are
  replaced with their lowercase counterparts; other characters are unchanged.
* `toupper(s)`: Returns a copy of `s` where all lowercase ASCII characters are
  replaced with their uppercase counterparts; other characters are unchanged.
* `exit [code]`: Exits the current process with the given code. `exit` attempts
  to flush any open file buffers. For parallel scripts, other worker threads
  have inputs cut off. Once those threads exit their main loop the process
  exits with the given exit code. This means that scripts with long loop
  iterations may not exit immediately. `exit` can be called with and without
  parentheses.
* `assert(cond, msg)`: If `cond` is false (tested as by `if`), prints `msg` to
  stderr and exits at once with code `2`. Unlike `exit 2`, the `END` block is
  not run (in serial and parallel mode alike), e.g.
  `{ assert(NF == 5, f"line {NR}: expect 5 fields, got {NF}") }`.

# Other Functions

* `close(s)` flushes all pending output to file `s` and then closes it.
* `length(x)` returns the length of `x`, where `x` can be either a string or an
  array.
* `system(s)` runs the command contained in the string `s` in a subshell,
  returning the error code, or the integer `1` if an error code was
  unavailable.

