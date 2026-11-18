# A numeric subscript is converted with CONVFMT when the element is created; changing CONVFMT
# or the variable later does not change the stored key (gawk arynasty, ofmta).
BEGIN {
    a = 12.153
    test[a] = "hi"
    CONVFMT = "%.0f"
    a = 5
    for (i in test) printf "test[%s] = %s\n", i, test[i]
    print (12.153 in test), ("12.153" in test), ("12" in test)
    test[12.153] = "new"; for (i in test) n++; print n
}
