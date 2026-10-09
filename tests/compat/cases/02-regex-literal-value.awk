# stdin: data/emp.data
# A regex literal used as a value is `$0 ~ /re/`; it stays a pattern after `~` and in builtins.
function f(x) { return x }
{
    n = /a/ + /e/; x = /a/
    c[/a/]++
    s = $0; k = sub(/a/, "A", s)
    print n, x, f(/a/), (/a/ ? "y" : "n"), !/a/, ($0 ~ /a/), k, s
}
END { print c[0] + 0, c[1] + 0, split("a1b22c", p, /[0-9]+/), match("foobar", /o+/), RLENGTH }
