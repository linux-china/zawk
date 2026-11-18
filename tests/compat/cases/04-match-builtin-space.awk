# A built-in function may be called with a space before `(`; an empty match is at position 1
# with RLENGTH 0 (gawk match4).
BEGIN {
    print match (m, /a?/), RSTART, RLENGTH
    print length ("abc"), substr ("hello", 2, 3), index ("abc", "c")
    print match ("xyz", /q*$/), RSTART, RLENGTH
}
