# The arguments of print are evaluated before anything is printed (gawk prtoeval, prt1eval).
function noisy() { print "<in function>"; return "'A STRING'" }
function tst() { sum += 1; return sum }
BEGIN {
    print "partial line:", noisy()
    OFMT = "%.0f"; print tst(), tst() + 0.25
}
