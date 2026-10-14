# split() with a fourth argument: seps[i] is the separator after arr[i]; leading and trailing
# blanks with the default separator go to seps[0] and seps[n].
function dump(n, a, s,    i, o) {
    o = n ":"
    for (i = 0; i <= n; i++) {
        if (i in s) o = o " s" i "=[" s[i] "]"
        if (i in a) o = o " a" i "=[" a[i] "]"
    }
    print o, length(s)
}
BEGIN {
    n = split("  a b\t c  ", a, " ", s); dump(n, a, s)
    n = split("   ", a, " ", s); dump(n, a, s)
    n = split("", a, " ", s); dump(n, a, s)
    n = split("a,,b,", a, ",", s); dump(n, a, s)
    n = split("x.y", a, ".", s); dump(n, a, s)
    n = split("a12b3c", a, /[0-9]+/, s); dump(n, a, s)
    print (s[1] == 12.0), (s[1] < 9)
    n = split("abc", a, "", s); dump(n, a, s)
    n = split("abc", a, "x", s); dump(n, a, s)
    FS = ":"
    n = split("p:q", a, FS, s); dump(n, a, s)
    # Rebuilding a line from the parts keeps the original separators.
    line = "k1 = v1 ;  k2=v2"
    n = split(line, a, /[=;]/, s)
    out = a[1]
    for (i = 1; i < n; i++) out = out s[i] a[i + 1]
    print (out == line), n
    # String-keyed arrays: old contents are cleared.
    m["old"] = 1; t["old"] = 1; t["x"] = "y"
    n = split("u-v", m, "-", t); dump(n, m, t)
}
