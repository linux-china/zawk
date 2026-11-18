# isarray() and typeof() for strnum fields, numbers, strings, regex constants and arrays
# (gawk typeof1..typeof5).
BEGIN {
    $0 = "12 abc 1e3 0x1A"
    print typeof($1), typeof($2), typeof($3), typeof(1), typeof("1"), typeof(1 "")
    a["x"] = 1; print isarray(a), isarray(a["x"]), isarray(nosuch), typeof(a), typeof(a["x"])
    n = split("3 x", s); print typeof(s[1]), typeof(s[2]), typeof(s[1] + 0)
}
