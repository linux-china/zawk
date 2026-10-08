# stdin: data/emp.data
# print/printf arguments may use any operator except an unparenthesized `>`, which redirects.
NR <= 3 {
    print $1 == "Beth" ? "beth" : "other", $2 != 0, $3 < 10, $3 >= 15, NR <= 1
    print $1 ~ /^K/, $1 !~ /^K/, ($1 in seen), NR == 1 && $2 >= 4, NR == 2 || NR == 3
    printf "%s %s\n", ($3 > 10) ? "many" : "few", ($2 > 4)
    seen[$1]
}
END { print 1 " " 2 < 3, x = 5, x }
