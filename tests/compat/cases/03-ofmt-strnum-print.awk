# print uses OFMT for numbers, but a strnum (field, split element) prints its original text
# (gawk numstr1, ofmtstrnum, strnum2).
BEGIN {
    split("1.234", f)
    OFMT = "%.1f"
    print f[1]
    x = f[1] + 0
    print f[1], x
    split(" 1", g, "|"); print "[" g[1] "]"; y = g[1] + 0; print "[" g[1] "]", y
    CONVFMT = "%.2f"; z = 3.14159; print z, (z "")
}
