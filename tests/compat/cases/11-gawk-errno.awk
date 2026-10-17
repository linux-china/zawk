BEGIN {
    print "[" ERRNO "]", typeof(ERRNO)
    print (getline x < "/nonexistent/zawk-errno"), "[" ERRNO "]"
    # A successful getline does not clear ERRNO.
    print (getline x < "data/emp.data"), "[" ERRNO "]"
    ERRNO = ""
    print (getline x < "data"), "[" ERRNO "]"
    print close("never-opened"), "[" ERRNO "]"
    ERRNO = "user"
    print "[" ERRNO "]"
}
