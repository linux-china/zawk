# asort(), asorti() and PROCINFO["sorted_in"] compare strings ignoring case when IGNORECASE is
# set; strings equal ignoring case are ordered by their bytes.
function show(arr, n,    i, s) {
    s = ""
    for (i = 1; i <= n; i++) s = s " " arr[i]
    print s
}
BEGIN {
    IGNORECASE = 1
    a["b"] = "B"; a["a"] = "a"; a["c"] = "C"; a["d"] = "_"; a["e"] = "A"; a["f"] = 10; a["g"] = 2
    show(v, asort(a, v))
    k["_"]; k["b"]; k["B"]; k["a"]; k["A"]
    show(r, asorti(k, r))
    m["B"] = "x"; m["a"] = "Y"; m["C"] = "z"
    PROCINFO["sorted_in"] = "@ind_str_asc"
    s = ""; for (i in m) s = s " " i; print s
    PROCINFO["sorted_in"] = "@val_str_desc"
    s = ""; for (i in m) s = s " " m[i]; print s
    delete PROCINFO["sorted_in"]
    IGNORECASE = 0
    show(v, asort(a, v))
}
