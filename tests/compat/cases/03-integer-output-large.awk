# Integral values print as integers even beyond the precision of OFMT; others use OFMT.
BEGIN {
    print 2^31, 2^32 + 1, 2^53, 1e15, 1e16, 123456789 * 1000
    print 2^53 / 2, -2^40, 1e6 / 4, (1e100 > 1e99)
    x = 0.1 * 3; print x, (x == 0.3)
}
