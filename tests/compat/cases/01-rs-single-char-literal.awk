# A single-character RS is matched literally, even when it is a regex metacharacter;
# longer values of RS are regexes. Also applies to `cmd | getline`.
BEGIN {
    n = split("| . * + ? ( [ \\ ^ $ { a", seps, " ")
    for (i = 1; i <= n; i++) {
        RS = seps[i]
        cmd = "printf 'x" seps[i] "y" seps[i] "z'"
        out = ""
        while ((cmd | getline line) > 0)
            out = out "[" line "]"
        close(cmd)
        print "RS=" seps[i], out
    }
    RS = "b+"
    cmd = "printf 'abbbcbd'"
    out = ""
    while ((cmd | getline line) > 0)
        out = out "[" line "]"
    close(cmd)
    print "RS=b+", out
}
