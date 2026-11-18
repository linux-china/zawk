# gsub with anchored and alternated regexes on $0 (gawk gsubtst2, gsubtst4, anchgsub).
function try(re) { $0 = "12345"; n = gsub(re, "x"); printf "%-10s %d %s\n", re, n, $0 }
BEGIN {
    try("(^)"); try("($)"); try("(^)|($)"); try("($)|(^)"); try("2"); try("(^)|2"); try("2|(^)")
    try("^1"); try("5$"); try("^[0-9]"); try("[0-9]$")
    $0 = "  \t leading blanks"; gsub(/^[ \t]*/, ""); print "[" $0 "]"
}
