# In an assignment to an array element, the value is evaluated before the subscript when the
# value calls a builtin with side effects: match sets RSTART/RLENGTH, split fills an array,
# srand reseeds the random number generator.
BEGIN {
    s = "xxab"; a[RSTART] = match(s, /ab/); for (k in a) print "match RSTART:", k, a[k]
    b[RLENGTH] = match("xxxy", /x+y/); for (k in b) print "match RLENGTH:", k, b[k]
    c[RSTART] = match("--abc", /(b)(c)/, m); for (k in c) print "match arr:", k, c[k], m[1] m[2]
    d[length(p)] = split("x y z", p); for (k in d) print "split:", k, d[k]
    srand(1); e[srand(2)] = srand(3); for (k in e) print "srand:", k, e[k]
    t = "aXbX"; n = 0; g[n + RSTART] += match(t, /X/); for (k in g) print "assign op:", k, g[k]
}
