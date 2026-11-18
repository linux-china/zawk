# split() deletes the old elements of the target array before filling it.
BEGIN {
    a["old"] = 1; a[5] = "five"
    n = split("x y", a)
    print n, length(a), ("old" in a), (5 in a), a[1], a[2]
    n = split("", a); print n, length(a)
}
