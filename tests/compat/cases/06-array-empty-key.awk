# The empty string is a valid subscript (gawk nlstrina).
BEGIN {
    v = ""
    ta[v]++
    if (v in ta) print "a", "[" v "]", ++ta[v], ta[v]
    print "b", ++ta[""], ta[uninit]
    for (i in ta) print "c", "[" i "]", ta[i]
    delete ta[""]; print length(ta)
}
