# Keywords directly followed by a non-identifier character, and identifiers starting with a keyword.
function g(x) { if (x) {n++; return} n += 10 }
BEGIN {
    while (1) {i++; if (i > 3) break}
    print "break}", i
    for (;;) {if (++j > 2) break}
    print "for break}", j
    k = 0; s = ""
    while (k < 3) {k++; if (k == 2) {continue}; s = s k}
    print "continue}", s
    getline<"data/a.txt"; print "getline<", $0
    getline line<"data/a.txt"; print "getline var<", line
    g(1); g(0); print "return}", n
    print"print\"", 1; printf"%s\n", "printf\""
    nextval = 1; exit_code = 2; elsewhere = 3; done = 4; iff = 5; format = 6; index_x = 7; delete_me = 8
    print nextval, exit_code, elsewhere, done, iff, format, index_x, delete_me
    a["x"] = 1; for(key in a)print "in", key
}
