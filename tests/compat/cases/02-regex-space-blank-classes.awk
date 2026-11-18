# [[:space:]] matches space, \t, \n, \v, \f, \r; [[:blank:]] only space and tab (gawk spacere).
BEGIN {
    c[1] = " "; c[2] = "\a"; c[3] = "\b"; c[4] = "\f"; c[5] = "\n"; c[6] = "\r"; c[7] = "\t"; c[8] = "\v"
    split("SP BEL BS FF NL CR TAB VT", name, " ")
    for (i = 1; i <= 8; i++)
        printf "%-3s space=%d blank=%d cntrl=%d \\s=%d\n", name[i], (c[i] ~ /[[:space:]]/), (c[i] ~ /[[:blank:]]/), (c[i] ~ /[[:cntrl:]]/), (c[i] ~ /\s/)
}
