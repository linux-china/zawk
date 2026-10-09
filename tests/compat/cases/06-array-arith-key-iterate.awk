# Arithmetic on numeric strings (`$1 + c`, `$1 % c`) used as array keys, then iterated.
BEGIN {
    n = split("1 2 3 4 5 6 7 8 9 10 3.5", v)
    for (i = 1; i <= n; i++) {
        $0 = v[i]
        m[$1 % 3]++; p[$1 + 0] = 1; x = 1 - $1; q[x]++; h[$1 / 2] = 1
    }
    for (k in m) { c++; s += k; t += m[k] }
    print c, s, t
    for (k in p) ps += k
    for (k in q) qs += k
    for (k in h) if (k == "0.5" || k == "1.75") half++
    print ps, qs, half
}
