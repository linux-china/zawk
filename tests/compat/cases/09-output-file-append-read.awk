# Writing a file, appending after close, and reading it back with getline.
BEGIN {
    f = "/tmp/zawk-compat-append.txt"
    print "one" > f
    print "two" > f
    close(f)
    print "three" >> f
    close(f)
    while ((getline line < f) > 0) print "read:", line
    close(f)
    printf "" > f; close(f)
    print (getline line < f)
    system("rm -f " f)
}
