# Unary plus and minus convert strings to numbers (gawk uplus, minusstr, uplus2).
BEGIN {
    print "01" + 0, +"01", -"01", -"6", +"3.50", -"-2", + "abc", -""
    print +a[y], +b[y], length(a)
    x = "  12  "; print -x, +x
}
