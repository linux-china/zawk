# Deep recursion works with both backends (the cranelift backend recurses on the native stack).
function rec(n) {
    if (n == 0)
        return 0
    return 1 + rec(n - 1)
}
function depth(n, s) {
    if (n == 0)
        return s
    return depth(n - 1, s "x")
}
BEGIN {
    print rec(1000000)
    print length(depth(20000, ""))
}
