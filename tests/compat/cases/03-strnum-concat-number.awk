# A string built by concatenation converts to a number when used numerically (gawk strnum1).
BEGIN {
    t = ""; t = t "8"
    printf "8  = %d\n", 0 + t
    t = t "8"
    printf "88 = %s\n", 0 + t
    u = "1"; u = u "e3"; print u + 0, u
}
