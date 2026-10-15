# patsplit(): the matches of the regex are the fields, the text around them the separators.
# seps[0] is the text before the first field and seps[i] the text after field i (kept even when
# empty); an empty match is a field unless it directly follows another match.
function dump(n, a, s,    i, o) {
    o = n ":"
    for (i = 0; i <= n; i++) {
        if (i in s) o = o " s" i "=[" s[i] "]"
        if (i in a) o = o " a" i "=[" a[i] "]"
    }
    print o, length(s)
}
BEGIN {
    n = patsplit("ab12cd3", a, /[0-9]+/, s); dump(n, a, s)
    n = patsplit("12ab3", a, /[0-9]+/, s); dump(n, a, s)
    n = patsplit("aaa,,bbb", a, /[^,]*/, s); dump(n, a, s)
    n = patsplit(",a,", a, /[^,]*/, s); dump(n, a, s)
    n = patsplit("", a, /x/, s); dump(n, a, s)
    n = patsplit("abc", a, /x/, s); dump(n, a, s)
    n = patsplit("a1b22c333", a, "[0-9]+"); print n, a[1], a[2], a[3], length(a)
    # The FPAT idiom for CSV with quoted fields.
    n = patsplit("x,\"y,z\",,w", a, /([^,]*)|("[^"]+")/)
    for (i = 1; i <= n; i++) print i, "[" a[i] "]"
    # Rebuilding the string from the parts keeps it intact.
    line = "k1=v1; k2 = v2"
    n = patsplit(line, a, /[a-z0-9]+/, s)
    out = s[0]
    for (i = 1; i <= n; i++) out = out a[i] s[i]
    print (out == line), n
    # String-keyed arrays: old contents are cleared.
    m["old"] = 1; t["old"] = 1
    n = patsplit("u-v", m, /[a-z]/, t); dump(n, m, t)
}
