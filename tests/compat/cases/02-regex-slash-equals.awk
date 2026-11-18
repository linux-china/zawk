# stdin: data/lines.txt
# A regex that starts with `=` after `,` or `(` is a regex, not the `/=` operator (gawk regeq,
# parsefld).
NR == 1 {
    print match("x=ab", /=a/), RSTART, RLENGTH
    n = split("p=q=r", parts, /=/); print n, parts[3]
    print /line/ + /1/ + !/x/
}
