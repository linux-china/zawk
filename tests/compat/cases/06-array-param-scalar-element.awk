# An array element passed to a function is a scalar by value; the function may still change the
# array through another array parameter (gawk aryprm, arrayparm).
function bump(x) { x++; return x }
function store(arr, k, v) { arr[k] = v }
BEGIN {
    a[1] = 5
    print bump(a[1]), a[1]
    store(a, 2, "two"); print length(a), a[2]
    store(fresh, "k", 1); print length(fresh), fresh["k"]
}
