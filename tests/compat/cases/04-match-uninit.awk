# match() on an uninitialized variable and with patterns matching the empty string (gawk match4,
# matchuninitialized).
BEGIN {
    print match(m, /a?/), RSTART, RLENGTH
    print match("xyz", /a*/), RSTART, RLENGTH
    print match("xyz", /q/), RSTART, RLENGTH
    print match("aaab", /a+b/), RSTART, RLENGTH
    print match("aaab", "b$"), RSTART, RLENGTH
}
