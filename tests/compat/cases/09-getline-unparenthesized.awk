# getline without parentheses: its result is assigned, printed and compared.
# stdin: data/a.txt
BEGIN {
    f = "data/b.txt"
    while (getline line < f > 0) n++
    print "lines:", n, line
    close(f)
    r = getline line < f; print "r:", r, line
    print getline line < f, line
    if (getline line < f == 1) print "eq:", line
    close(f)
    while ("printf \"1\\n2\\n3\\n\"" | getline > 0) s += $0
    print "sum:", s
    r = "echo x y" | getline v; print "cmd:", r, v
    print "missing:", getline x < "data/no-such-file"
}
NR == 1 { r = getline; print "main:", r, $0; while (getline > 0) m++; print "rest:", m + 0 }
