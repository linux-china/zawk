# `;` is an empty statement: in blocks, as a loop body and as an if/else branch.
function f() { ; }
BEGIN {
    ; ; print "a";; print "b"
    x = 3; while (x--) ; print x
    for (i = 0; i < 3; i++) ; print i
    for (k in arr) ; print "for-in"
    do ; while (j++ < 2); print j
    if (1) ; else print "no"; print "if"
    if (0) print "no"; else ; print "else"
    { } ; f(); print "end"
}
