# asorti(): sorts the indices into a destination array (or in place), with the predefined orders.
# stdin: data/asort-strnum.txt
BEGIN {
    a["b"] = 1; a["a"] = 2; a[10] = 3; a[2] = 4
    n = asorti(a)
    for (i = 1; i <= n; i++) printf "%s ", a[i]
    print n, length(a)

    b[3] = "z"; b[1] = "y"; b[20] = "x"
    n = asorti(b, c)
    for (i = 1; i <= n; i++) printf "%s ", c[i]
    print n, length(b), b[20]

    n = asorti(b, c, "@ind_num_desc")
    for (i = 1; i <= n; i++) printf "%s ", c[i]
    print n

    n = asorti(b, c, "@val_str_asc")
    for (i = 1; i <= n; i++) printf "%s ", c[i]
    print n

    v["x"] = 3.5; v["y"] = 1.5; v["z"] = 2
    n = asorti(v, w, "@val_num_desc")
    print n, w[1], w[2], w[3]

    old["stale"] = 1
    print asorti(empty, old), length(old), ("stale" in old)
}
{ count[$1]++ }
END {
    n = asorti(count, keys)
    for (i = 1; i <= n; i++) print keys[i], count[keys[i]]
}
