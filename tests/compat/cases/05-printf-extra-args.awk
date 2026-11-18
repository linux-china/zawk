# Extra printf arguments are ignored; a format can be reused from a variable.
BEGIN {
    printf "%s\n", "a", "b", "c"
    fmt = "%-4s=%3d\n"
    printf fmt, "x", 1
    printf(fmt, "yy", 22)
    s = sprintf("%s-%s", "p", "q", "r"); print s
    printf "no conversions\n", 1, 2
}
