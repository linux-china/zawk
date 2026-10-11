# typeof: a variable that is never read is "untyped", one read as a value is "unassigned".
function f(p) { return typeof(p) }
function g(p) { q = p; return typeof(p) }
BEGIN {
    print typeof(never), typeof(arr[1]), f(), f(passed), typeof(passed)
    print isarray(inspected), typeof(inspected)
    y = read; if (cond) ; z = elems[1]
    print typeof(read), typeof(cond), typeof(elems[1]), g()
    n = 1; s = "x"; a[1] = 1
    print typeof(n), typeof(s), typeof(a)
}
