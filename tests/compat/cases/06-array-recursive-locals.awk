# Each call of a recursive function gets fresh local arrays, and a local array can be passed down
# (gawk tailrecurse, fnarray2).
function abc(c, A, B) {
    print "abc(" c ", " length(A) ")"
    if (!c--) return
    B[""]; B[c]
    print length(B)
    return abc(c, B)
}
function depth(n,   loc) {
    loc[n] = n
    if (n > 0) depth(n - 1)
    return length(loc)
}
BEGIN { abc(2); print depth(5) }
