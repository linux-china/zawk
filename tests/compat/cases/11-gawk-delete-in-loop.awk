# Deleting the current element inside for-in is allowed; all elements end up deleted.
BEGIN {
    for (i = 1; i <= 50; i++) a[i] = i
    for (k in a) { delete a[k]; n++ }
    print n, length(a)
    split("x y z", b); for (k in b) delete b; print length(b)
}
