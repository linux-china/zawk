# Assignments in the branches of a conditional expression: `c ? y = 1 : y = 2`.
BEGIN {
    1 ? y = "eq" : y = "ne"; print y
    0 ? y = "eq" : y = "ne"; print y
    x = 1 ? a = 1 : b = 2; print x, a, b
    x = 0 ? 1 : z = 9; print x, z
    n = 0; 1 ? n += 2 : n -= 2; print n
    v = 1 ? 2 ? 3 : 4 : 5; print v
    for (i = 0 ? j = 7 : k = 1; i < 3; i++) s = s i
    print s, j, k
}
