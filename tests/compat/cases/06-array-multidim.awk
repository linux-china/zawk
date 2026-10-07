BEGIN { a[1, 2] = "x"; print a[1, 2], ((1, 2) in a); for (k in a) { split(k, p, SUBSEP); print p[1], p[2] } }
