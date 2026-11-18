# A strnum used as a subscript keeps its original text ("007" is not "7"); a number is converted
# (gawk arrayind3, numindex).
BEGIN {
    split("007 7.0 1e2", f)
    pos[f[1]] = 1; pos[f[2]] = 2; pos[f[3]] = 3
    pos[7] = 4; pos[100] = 5
    PROCINFO["sorted_in"] = "@ind_str_asc"
    for (k in pos) print k, pos[k]
    print (7 in pos), ("7" in pos), ("007" in pos), (f[1] + 0 in pos), (1e2 in pos)
}
