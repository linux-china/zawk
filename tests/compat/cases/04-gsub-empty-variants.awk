# gsub with regexes that can match the empty string (gawk gsubtest, gsubtst6).
function attempt(re,    s, n) { s = "abc"; n = gsub(re, "X", s); print re, n, s }
BEGIN {
    attempt("b+"); attempt("x*"); attempt("b*"); attempt("c"); attempt("c+"); attempt("x*$"); attempt("b|$"); attempt("^x*")
    s = "hello"; n = gsub(//, "-", s); print n, s
    s = ""; n = gsub(/x*/, "-", s); print n, "[" s "]"
}
