# stdin: data/lines.txt
# `getline < file` sets $0 and NF but not NR/FNR; `getline var < file` changes only var; plain
# getline advances NR and FNR.
NR == 1 {
    getline < "data/a.txt"; print "file:", $0, NF, NR, FNR
    getline v < "data/a.txt"; print "var:", v, $0, NR
    getline; print "plain:", $0, NR, FNR
    getline w; print "plainvar:", w, $0, NR, FNR
}
END { print NR }
