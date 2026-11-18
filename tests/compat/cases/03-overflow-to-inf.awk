# Products and numeric strings beyond the double range become +inf/-inf (gawk inftest, numrange).
BEGIN {
    x = 100; k = 0
    do { k++; y = x; x *= 1000 } while (y < x && k < 400)
    print k, x, -x, (x == x + 1), (x > 1e308)
    n = split("-1.2e+931 1.2e+931 1e-400", a)
    for (i = 1; i <= n; ++i) print a[i], +a[i], -a[i]
    printf "%.2f %e %g %G\n", -1e308 * 10, 1e308 * 10, -1e308 * 10, 1e308 * 10
}
