# A function parameter used as an array subscript in a read keeps its value
# when called with numeric arguments (regression: integer keys became "").
function get(k) { return g[k] }
function inc(arr, k) { arr[k]++ }
function bump(arr, k) { arr[k] = arr[k] + 1; x = arr[k]; return x }
function only_int(arr, k) { arr[k]++; return arr[k] }
BEGIN {
    g["a"] = 6; g[1] = 5
    print get(1), get("a")

    inc(c, "a"); inc(c, "a"); inc(c, 1)
    print "c[a]=" c["a"], "c[1]=" c[1], ((1 in c) ? "has1" : "no1"), (("" in c) ? "hasEmpty" : "noEmpty")

    print bump(d, "a"), bump(d, 2), bump(d, 2)
    print "d[2]=" d[2], (("" in d) ? "hasEmpty" : "noEmpty")

    print only_int(e, 3), only_int(e, 3)
    for (k in e) print "e[" k "]=" e[k]
}
