# A bracket expression with many escaped punctuation characters (gawk gsubtst5).
BEGIN {
    s = "a b\"c-d/e\\f:g;h[i]j@k?l.m,n$o!p"
    gsub(/[\ \"\-\/\\:;\[\]\@\?\.\,\$]/, "", s)
    print s
    t = "x^y-z]w"; gsub(/[]^-]/, "_", t); print t
}
