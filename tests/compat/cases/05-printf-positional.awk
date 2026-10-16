BEGIN {
    printf "%2$s %1$s\n", "a", "b"
    printf "%1$s-%1$s\n", "x"
    printf "[%2$*1$d][%3$.*1$f]\n", 5, 42, 3.14159
    s = sprintf("%3$s/%2$s/%1$s", "d", "m", "y")
    print s
}
