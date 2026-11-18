# gsub with anchored and alternated regexes on $0 (gawk gsubtst2, gsubtst4, anchgsub).
function attempt(re) { $0 = "12345"; n = gsub(re, "x"); printf "%-10s %d %s\n", re, n, $0 }
BEGIN {
    attempt("(^)"); attempt("($)"); attempt("(^)|($)"); attempt("($)|(^)"); attempt("2"); attempt("(^)|2"); attempt("2|(^)")
    attempt("^1"); attempt("5$"); attempt("^[0-9]"); attempt("[0-9]$")
    $0 = "  \t leading blanks"; gsub(/^[ \t]*/, ""); print "[" $0 "]"
}
