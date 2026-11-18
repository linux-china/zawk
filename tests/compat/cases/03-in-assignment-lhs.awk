# The left side of `in` may be an assignment, which is performed (gawk intest).
BEGIN {
    r = ((b = 1) in c); print r, b, length(c)
    c[2]; r = ((b = b + 1) in c); print r, b
}
