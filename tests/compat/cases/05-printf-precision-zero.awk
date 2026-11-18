# An explicit zero or small precision with `#` on zero, `#` with float conversions of zero, and
# integer conversion of values between -1 and 0 (gawk printf-corners, zero2).
BEGIN {
    printf "<%#.o><%#.1o><%#.2o><%#.3o><%#.2x>\n", 0, 0, 0, 0, 1
    printf "<%.0d><%+.0d><%.u><%.o><%.x>\n", 0, 1, 0, 0, 0
    printf "<%.s><%.0s><%.1s><%.10s>\n", "abc", "abc", "abc", "abc"
    printf "<%#g><%#G><%#.e><%#.E><%#.f><%#.g>\n", "0", "0", "0", "0", "0", "0"
    printf "<%d><%d><%d><%i><%.0f>\n", -.4, -0.0, -.9, -0.5, -0.4
}
