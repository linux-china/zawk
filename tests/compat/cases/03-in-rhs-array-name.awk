# The right operand of `in` is an array name: `k in seen == 0` is `(k in seen) == 0`.
BEGIN {
    print 1 in a == 0
    a[1]
    if (1 in a && 2 in a == 0) print "ok"
    print 1 in a ~ 1, 2 in a != 1
    m[1, 2]
    print (1, 2) in m, (1, 3) in m == 0
    b[1]
    print 1 in a in b
    x = 1
    print x == 1 in a
    n = split("a b a c b", w, " ")
    for (i = 1; i <= n; i++) if (w[i] in seen == 0) { seen[w[i]]; printf "%s ", w[i] }
    print ""
}
