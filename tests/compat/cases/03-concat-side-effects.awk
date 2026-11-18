# Operands are evaluated left to right: a function called later in a concatenation or an
# argument list that modifies a global does not change the value already read
# (gawk nasty, nasty2, concat2, concat3, concat5, prtoeval, prt1eval).
function mod() { gsub(/a/, "123", g); return "X" }
function locals(s, x) { x = 1; s = 3; s = s x; return s }
function say() { print "<in function>"; return "'A STRING'" }
function tst() { sum += 1; return sum }
BEGIN {
    g = "aaaa"; g = g "|" mod(); print g
    g = "aaaa"; printf "g = %s, mod() = %s\n", g, mod(); print g
    g = "ab"; h = g mod() g; print h
    for (i = 1; i <= 3; i++) printf "%s ", locals(); print ""
    a = a (b "c"); print a; print "[" b "]"
    OFMT = "%.8g"; x = 1; x += .1; x = (x "a"); print x
    print "partial line:", say()
    OFMT = "%.0f"; print tst(), tst() + 0.25
}
