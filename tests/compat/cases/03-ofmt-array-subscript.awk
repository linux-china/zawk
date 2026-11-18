# A non-integral numeric subscript is converted with CONVFMT, not OFMT (gawk ofmta, numindex).
BEGIN {
    i = 3 + 1.2345
    a[i] = "hi"
    OFMT = "%.1f"
    print i
    for (k in a) print k, a[k]
    print a[i]
    CONVFMT = OFMT = "%.3f"
    print i, (i in a)
    b[0.1 + 0.2] = 1; for (k in b) print k
    c[12] = 1; c[12.0] = 2; c["12"] = 3; print length(c), c[12]
}
