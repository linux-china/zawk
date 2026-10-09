# Unary operators on variables that are never assigned (0 and "" at once), and `!` on floats.
function neg(a) { return -a }
function not(a) { return !a }
BEGIN {
    if (!x) print "unset"
    print -x, +y, !z, !!w, !a[1], -!u
    v = -x; print "[" v "]", v + 1
    p = q++; print p, q
    print neg(), neg(2), not(), not(0), not("")
    r = 0.5; print !r, !(0.25), !0.0, !(1 - 1.0), !-0.5
    if (!r) print "zero"; else print "nonzero"
}
