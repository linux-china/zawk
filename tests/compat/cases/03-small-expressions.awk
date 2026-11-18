# Small expressions from bug reports: unary minus of a string, a negative exponent, an octal
# looking subscript, a numeric SUBSEP, `/=` on an element, string division, numeric comparisons
# of a subscript variable and substr() of a sum (gawk minusstr, negexp, octsub, subsepnm,
# subslash, divzero2, arysubnm, numsubstr, leaddig).
BEGIN {
    print -"6", -"-3x", +"4.5e1"
    a = -2; print 10^a, 2^-1
    ++x[03]; print "/" x[0] "/" x[3] "/" ("03" in x) "/"
    SUBSEP = 10; m[1, 1] = 100; print m[1 SUBSEP 1], m["1101"]
    i = 2; b[i] = 5; b[i] /= 2; printf "b[%s] = %f\n", i, b[i]
    print "2" / "3", "6" * "7", "10" % "4"
    n = 11; c[n] = n; print (2 <= n), (n in c), ("11" in c)
    print substr(1000 + 5000, 2), substr(1000 + 10000, 2), substr(-1.5 * 2, 1, 2)
    y = "2E"; print y, (y == 2), (y == 2E0), (y + 0 == 2)
}
