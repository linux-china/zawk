# Output to a pipe appears when the pipe is closed; system() and close() flush pending output
# first.
BEGIN {
    print "first"
    print "c" | "sort"; print "a" | "sort"; print "b" | "sort"
    close("sort")
    print "after sort"
    system("echo from system")
    print "last"
    printf "x\n" | "cat"
    close("cat")
    print "end"
}
