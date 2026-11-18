# index() and length() see strings changed by concatenation, sub() and getline (gawk concat4,
# wideidx, wideidx2).
BEGIN {
    v = "abc"; print index(v, "bc"); sub(/bc/, "bbc", v); print v, index(v, "bc"), length(v)
    a = "xxx"; print index(a, "b"); a = a "yb"; print index(a, "b"), index(a, ""), index("", "a")
    print index("hello", "lo"), index("hello", "hello!"), index(12345, 34)
}
