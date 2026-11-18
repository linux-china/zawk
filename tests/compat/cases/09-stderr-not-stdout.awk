# Output to /dev/stderr and to a `cat 1>&2` pipe does not reach standard output.
BEGIN {
    print "to stderr" > "/dev/stderr"
    print "to stderr too" | "cat 1>&2"
    close("cat 1>&2")
    printf "%s\n", "to stdout"
    print "also stdout" > "/dev/stdout"
}
