# Awk numbers are doubles: integer arithmetic never wraps around or fails on overflow.
BEGIN {
    print 9223372036854775807 + 1, -9223372036854775807 - 2, 9007199254740993, 0x7fffffffffffffff
    a = 0; b = 1; for (i = 0; i < 100; i++) { c = a + b; a = b; b = c }
    print b
    x = 1; for (i = 0; i < 70; i++) x += x
    m[1] = 1; for (i = 0; i < 70; i++) m[1] += m[1]
    y = -1; for (i = 0; i < 70; i++) y = y - (-y)
    print x, m[1], y
    # Sums used as subscripts and counters behave as before.
    for (i = 0; i < 3; i++) for (j = 0; j < 3; j++) k[i + j]++
    print length(k), k[2], ((1 + 1) in k), i + 1, i - 1
}
