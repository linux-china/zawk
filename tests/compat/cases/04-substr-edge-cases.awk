# substr with out-of-range and non-integral positions and lengths (gawk substr, numsubstr).
BEGIN {
    printf "%-10s|\n", substr("A", 1, 39)
    print "[" substr("abcdef", 0, 2) "]", "[" substr("abcdef", 2.3, 2) "]", "[" substr("abcdef", -1, 2) "]"
    print "[" substr("abcdef", 1, 0) "]", "[" substr("abcdef", 1, -3) "]", "[" substr("abcdef", 1, 2.3) "]"
    print "[" substr("", 1, 2) "]", "[" substr("abcdef", 5, 5) "]", "[" substr("abcdef", 7, 2) "]"
    print "[" substr("abcdef", 2.7) "]", "[" substr("abcdef", -5, 8) "]", "[" substr(12345, 2, 3) "]"
    print substr(1000 + 7, 2), substr(1000 + 42, 2)
}
