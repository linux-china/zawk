# An uninitialized numeric variable or missing array element passed to a user-defined function.
function show(s) { return "<" s ">" }
BEGIN {
    if (0) u++
    a[1] = 5
    print show(u), show(a[2])
}
