# stdin: data/numbers.txt
# getline and getline var update NR and FNR; reading a file or command does not. A failed
# getline (end of input, missing file) leaves its target unchanged.
NR == 1 {
    getline; print "plain", NR, FNR, $0
    r = (getline x); print "var", r, NR, FNR, x, $0
    "echo cmd" | getline c; print "cmd", c, NR
    getline f < "data/a.txt"; print "file", NR
    v = "keep"; r = (getline v < "data/no-such-file"); print "missing", r, v
    while ((getline y) > 0) n++
    print "rest", n, NR, "[" y "]", $0
}
END { print "end", NR, $0 }
