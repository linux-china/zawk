# An uninitialized variable or field used as a subscript is the empty string key, not "0"
# (gawk aryunasgn).
BEGIN {
    a[i] = "null"
    for (k in a) { print length(k), a[k]; print (k == 0), (k == "") }
    print a[""], "[" a[0] "]", ("0" in a), ("" in a)
    b[$2] = "null also"
    for (k in b) print length(k), (k == "")
    c[u + 0] = "zero"; for (k in c) print "[" k "]"
}
