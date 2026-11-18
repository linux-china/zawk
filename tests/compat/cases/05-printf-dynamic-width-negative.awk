# A negative `*` width means left justification; `*` precision works for strings and numbers
# (gawk dynlj).
BEGIN {
    printf "%*sworld\n", -20, "hello"
    printf "[%*d]\n", 6, 42
    printf "[%-*d]\n", 6, 42
    printf "[%.*s]\n", 3, "abcdef"
    printf "[%*.*f]\n", 10, 2, 3.14159
}
