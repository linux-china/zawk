# POSIX character classes inside bracket expressions.
BEGIN {
    s = "aZ9 _\t.!"
    n = split("alpha digit alnum upper lower space blank punct xdigit print graph cntrl", cls, " ")
    for (i = 1; i <= n; i++) {
        t = s
        c = gsub("[[:" cls[i] ":]]", "", t)
        printf "%s=%d ", cls[i], c
    }
    print ""
    print ("F" ~ /^[[:xdigit:]]$/), ("g" ~ /^[[:xdigit:]]$/), ("a-1" ~ /^[[:alpha:]-][^[:alpha:]]+$/)
}
