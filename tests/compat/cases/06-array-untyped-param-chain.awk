# An array first used through a chain of function parameters is created in the caller, also when
# the inner function only creates an element or calls split() (gawk arrayref, arryref2, arrayprm2,
# arrayprm3, delarpm2, arrymem1).
function test(arr) { test2(arr) }
function test2(arr2) { arr2[1] = 1 }
function viasplit(baz) { split("x y", baz) }
function hasempty(a) { mkempty(a); return ("" in a) }
function mkempty(a) { a[""] }
function foo(b) { bar(b); b[2] = "local" }
function bar(c) { g[3] = "global"; c[1] = "local2" }
function clear(array, key) { for (key in array) delete array[key] }
function fill(a) { a[1] = "one"; a[2] = "two" }
function setglobal() { gl[""] = "b" }
function outer(p, q) { q[""] = "a"; setglobal() }
BEGIN {
    test(t1); print t1[1], length(t1)
    viasplit(t2); print length(t2), t2[2]
    print hasempty(t3)
    foo(g); for (i = 1; i <= 3; i++) print i, g[i]
    clear(table); fill(table); for (i = 1; i <= 2; i++) print i, table[i], "<" table[k] ">"
    clear(table); print length(table)
    outer(procs, lb); print "[" gl[""] "]", length(procs)
}
