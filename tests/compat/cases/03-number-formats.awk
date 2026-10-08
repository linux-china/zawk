# stdin: data/emp.data
# print uses OFMT and conversions to strings use CONVFMT (both "%.6g" by default) for
# non-integral values; integral values print as integers with all digits.
BEGIN {
    print 0.1 + 0.2, 1/3, 1234567.5, 1e17, 2^63, 1e30, -2.5
    print 1e300 * 1e300, -1e300 * 1e300, 0 * -1
    OFMT = "%.2f"; CONVFMT = "%.3g"; x = 3.14159
    print x, x "", (x " "), 2.0
    a[x] = 1; for (k in a) print "key", k
    CONVFMT = "%d"; print 3.7 ""
    OFMT = "%.6g"; CONVFMT = "%.6g"
}
NR <= 2 { $4 = $2 * 1.1; print }
