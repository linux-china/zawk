# split() with a single space, a literal space regex, a dot regex, a dynamic regex string, and
# a source taken from the target array (gawk splitwht, splitany, splitvar, splitarr).
BEGIN {
    str = "a   b\t\tc d"
    print split(str, a, " "), split(str, b, / /), split(str, c, "\t")
    n = split("xyz", d, /./); print n, "[" d[1] "]", "[" d[4] "]"
    sep = "=+"; print split("a==b=c", e, sep), e[2]
    print split("a.b.c", g, "."), g[3]
    h[1] = "elephantie"; h[2] = "e"
    print split(h[1], h, h[2]), h[2], h[3], length(h)
    print split("abc", k, ""), k[1], k[3]
}
