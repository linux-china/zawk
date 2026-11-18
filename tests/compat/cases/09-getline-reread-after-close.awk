# close() of a file read with getline lets it be read again from the start, in a loop in END
# (gawk redfilnm).
END {
    file = "data/three-lines.txt"
    for (i = 1; i <= 3; i++) {
        n = 0
        while ((getline < file) > 0) n++
        print i, n, $0, NR
        close(file)
    }
}
