# `^` and `$` in the middle of a regex are anchors, so these regexes never match (gawk tradanch).
BEGIN {
    print ("foo^bar" ~ /foo^bar/), ("foo$bar" ~ /foo$bar/), ("foo^bar" ~ /foo\^bar/), ("x$" ~ /x\$/)
    print ("ab" ~ /(^a)b/), ("ab" ~ /a(b$)/), ("ab" ~ /^(a|b)+$/)
}
