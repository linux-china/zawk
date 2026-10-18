# A backslash-newline inside a string or regex literal is a line continuation (removed).
BEGIN {
    s = "x\
y"
    print s, length(s)
    print "ab" ~ /a\
b/
    print "one \
two" ~ /e t/
    print "a/b" ~ /a\
\/b/
    printf "%s\
\n", "p"
    print (s, "z\
w")
}
