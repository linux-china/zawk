# sub() with `^` anchored regexes consumes a string piece by piece; each call only looks at the
# current start (gawk subi18n, widesub).
BEGIN {
    str = "type=\"directory\" version=\"1.0\" name=\"x y\""
    while (str) {
        sub(/^[^=]*/, "", str)
        print substr(str, 2)
        sub(/^="[^"]*"/, "", str)
        sub(/^[ \t]*/, "", str)
    }
    s = "aaa"; n = 0
    while (sub(/^a/, "", s)) n++
    print n, "[" s "]"
}
