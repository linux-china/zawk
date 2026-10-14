# stdin: data/asort-strnum.txt
# asort() sorts numbers before strings. String constants (and copies of them) are strings even
# when they look numeric, so they sort as strings; strnums (fields, split elements) that look
# numeric sort as numbers.
{ f[NR] = $1 }
END {
    a[1] = "10"; a[2] = "9"; a[3] = "x"; a[4] = "2"
    n = asort(a); for (i = 1; i <= n; i++) printf "%s ", a[i]; print n
    c[1] = "10"; c[2] = "9"; asort(c, d); asort(d, e); print e[1], e[2]
    n = split("10 9 x 2", s); asort(s); print s[1], s[2], s[3], s[4]
    n = asort(f, g); for (i = 1; i <= n; i++) printf "%s ", g[i]; print n
}
