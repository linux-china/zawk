# \<, \>, \y and \B treat `_` as a word character (gawk gnuops2, gnuops3).
BEGIN {
    print match("X _abc Y", /\<_abc/), match("X _abc Y", /\y_abc/)
    print match("X abc_ Y", /abc_\>/), match("X abc_ Y", /abc_\y/)
    print match("X abc_def Y", /abc_\Bdef/), match("X a_c Y", /a\wc/), match("X a.c Y", /a\Wc/)
    print ("  " ~ / \B /), ("a b" ~ /\B/), (" b" ~ /\B/), ("a " ~ /\B/)
    a = "  "; gsub(/\B/, "x", a); print a
}
