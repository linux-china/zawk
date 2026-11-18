# stdin: data/gensub-quotes.txt
# gensub() with escaped backslashes in the replacement, \\1 with bracket classes, and \\0
# (gawk gensub4, gensub5; a lone trailing backslash is left out: gawk 5.1 outputs a NUL).
BEGIN {
    print gensub(/(.)/, "\\\\\\1", "g", "ab"), gensub(/b/, "\\\\&", 1, "abc")
}
{ print gensub(/"([^[:blank:]]*)"/, "[\\1]", "g", $0), gensub(/"([^"]*)"/, "<\\0>", 2) }
