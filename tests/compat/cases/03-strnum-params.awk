# repeat: 30
# Strnum-ness flows through function arguments: a string parameter or the elements of an array
# parameter are strnums when the caller passes strnums, and arrays are passed by reference.
# Parameters of other types must not affect unrelated string values (the strnum analysis once
# confused registers of different types, depending on the order of a hash map).
function g(arr) { return (arr[1] ? "t" : "f") (arr[2] ? "t" : "f") }
function g2(arr) { return (arr[1] ? "t" : "f") (arr[2] ? "t" : "f") }
function h(s) { return s ? "t" : "f" }
function h2(s) { return s ? "t" : "f" }
function fill(arr) { split("0 1", arr) }
function n(x) { return x + 1 }
BEGIN {
    split("0 1", a); print g(a), h(a[1])
    b[1] = "0"; b[2] = "1"; print g2(b), h2("0"), h2(0 "")
    fill(c); print (c[1] ? "t" : "f"), (c[2] ? "t" : "f")
    print n(1), (("0" "") ? "t" : "f"), (substr("0", 1) ? "t" : "f")
}
