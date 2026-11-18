# close() returns the exit status of a command, 0 for a file, and -1 for something not open
# (gawk close_status, status-close, closebad).
BEGIN {
    cmd = "echo x; exit 3"
    cmd | getline v
    print v, close(cmd)
    out = "cat; exit 2"
    print "piped" | out
    print close(out)
    print close("never-opened")
    print "ok" | "cat"; print close("cat")
    f = "data/lines.txt"; getline line < f; print line, close(f)
}
