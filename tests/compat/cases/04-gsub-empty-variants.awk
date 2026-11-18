# gsub with regexes that can match the empty string (gawk gsubtest, gsubtst6).
function try(re,    s, n) { s = "abc"; n = gsub(re, "X", s); print re, n, s }
BEGIN {
    try("b+"); try("x*"); try("b*"); try("c"); try("c+"); try("x*$"); try("b|$"); try("^x*")
    s = "hello"; n = gsub(//, "-", s); print n, s
    s = ""; n = gsub(/x*/, "-", s); print n, "[" s "]"
}
