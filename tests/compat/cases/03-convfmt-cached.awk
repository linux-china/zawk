# CONVFMT applies when a number is converted to a string; integral values are converted as
# integers (gawk convfmt, POSIX).
BEGIN {
    CONVFMT = "%2.2f"
    a = 123.456
    b = a ""
    printf "a = %s\n", a
    CONVFMT = "%.6g"
    printf "a = %s\n", a
    a += 0
    printf "a = %s\n", a
    CONVFMT = "%.2f"; n = 12; print (n ""), (12.5 ""), (-3 ""), (1e3 "")
}
