# `printf "" > f` opens the file, so it creates or truncates it even though nothing is written:
# the usual way to empty a file before appending to it with `>>`.
BEGIN {
    f = "/tmp/zawk-compat-empty-" PROCINFO["pid"]
    print "old" > f
    close(f)
    printf "" > f
    close(f)
    n = 0
    while ((getline line < f) > 0) n++
    close(f)
    print "after printf \"\":", n
    for (i = 0; i < 3; i++) {
        printf "%s", "" > f
        close(f)
        print "run", i >> f
        close(f)
    }
    while ((getline line < f) > 0) print line
    close(f)
    system("rm -f " f)
    g = "/tmp/zawk-compat-new-" PROCINFO["pid"]
    printf "" > g
    print "created:", (system("test -f " g) == 0)
    system("rm -f " g)
}
