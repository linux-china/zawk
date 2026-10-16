BEGIN {
    print "a\qb", "x\/y"
    n = split("/usr/local/bin", p, "\/")
    print n, p[2], p[4]
    print "tab[\t] slash[\\] oct[\101] hex[\x41]"
}
