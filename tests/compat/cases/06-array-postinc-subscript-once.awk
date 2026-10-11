# The subscript of a[k]++ / a[k]-- is evaluated only once.
BEGIN {
    i = 0
    a[i++]++
    print i, a[0], (1 in a)
    x = b[i--]--
    print i, x, b[1]
    c["k"] = "v"
    y = c["k"]++
    print y, c["k"]
    s = 0
    d[n = n + 1]++
    print n, d[1]
    for (j = 7; j < 1000007; j += 99999) e[j % 100]++
    for (j = -7; j > -1000007; j -= 99999) e[j % 100]++
    print length(e)
    print 7.5 % 2, -7.5 % 2, -4 % 2, 1e15 % 7, 2^60 % 3
}
