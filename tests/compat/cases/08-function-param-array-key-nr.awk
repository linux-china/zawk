# stdin: data/a.txt
# Array subscript parameter called with a string, a field and NR.
function inc(arr, k) { arr[k]++ }
function get(arr, k) { return arr[k] }
{ inc(c, "a"); inc(c, $1); inc(c, NR) }
END {
    n = 0
    for (k in c) n++
    print "keys=" n, (("" in c) ? "hasEmpty" : "noEmpty")
    print "a=" get(c, "a"), "1=" get(c, 1), "2=" get(c, NR - 1)
}
