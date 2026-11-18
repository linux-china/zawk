# The subscript of a compound assignment is evaluated once (gawk opasnidx, rebuild).
BEGIN {
    b = 1; a[b] = 2; a[b++] += 1; print b, a[1], a[2] == ""
    i = 1; c[i++] *= 5; print i, length(c)
    j = 0; d[++j] = j; print j, d[1]
}
