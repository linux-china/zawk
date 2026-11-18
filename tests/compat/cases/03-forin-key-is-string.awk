# The variable of `for (k in a)` holds a string, even for numeric-looking keys: it compares as a
# string and %c prints its first character (gawk strsubscript, printfchar).
BEGIN {
    a["10"]
    for (i in a) { if (i < 2) print "string compare", i; print i + 0 < 2 }
    x[65]
    for (k in x) printf "%c|\n", k
}
