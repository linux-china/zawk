# `>` truncates the file when it is first opened, even if `>>` is used for the same file later
# while it is still open (both redirections share one stream).
BEGIN {
    f = "/tmp/zawk-compat-trunc-" PROCINFO["pid"]
    printf "old1\nold2\n" > f
    close(f)
    print "new" > f
    print "appended" >> f
    close(f)
    while ((getline line < f) > 0) print line
    close(f)
    system("rm -f " f)
}
