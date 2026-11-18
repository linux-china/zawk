# Splitting on the string "^" does not split, since `^` only matches at the start
# (gawk splitwht2; the regex form `/^/` is left out: gawk 5.1 drops the first character).
BEGIN {
    str = "ABCDE"
    n = split(str, arr, "^"); print str, n, arr[1]
    n = split("a^b^c", arr, "\\^"); print n, arr[1], arr[3]
    n = split("a^b", arr, /\^/); print n, arr[2]
}
