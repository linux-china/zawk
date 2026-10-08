# fflush() and fflush("") flush all output; fflush(name) returns 0 for an open output file or
# command and -1 otherwise (also after it was closed).
BEGIN {
    printf "a"; r1 = fflush(); system(""); print "b", r1, fflush("")
    print "x" | "cat"; print fflush("cat"); close("cat"); print fflush("cat")
    print fflush("/dev/stdout"), fflush("never opened")
}
