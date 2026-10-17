# stdin: data/emp.data
# Builtin function names may be followed by blanks before `(` (POSIX); user functions may not.
NR == 1 {
    x = 3.7
    print int (x), sqrt (16), toupper ("abc"), tolower	("ABC")
    print length ($1), length ("abc"), length
    print substr ($0, 2, 4), index ("hello", "l")
    n = split ("a b c", parts); print n, parts[3]
    s = "aaa"; gsub (/a/, "b", s); print s
    print sprintf ("%05.1f", x)
}
