# stdin: data/a.txt
# getline into a field rebuilds $0 and updates NF.
NR == 1 {
    "echo X" | getline $2; print; print NF
    "echo Y Z" | getline $5; print; print NF, $5
    i = 1; getline $i < "data/b.txt"; print; print NF
    close("data/b.txt"); getline $0 < "data/b.txt"; print $1, NF
    getline $3; print; print NF, NR
    while (("echo a; echo b" | getline $1) > 0) n++; print n, $0
}
