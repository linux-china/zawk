# stdin: data/float-truthiness.txt
# Floats are true unless they equal zero: |x| < 1 and NaN are true, 0.0 and -0.0 are false.
function t(x) { return x ? "T" : "F" }
BEGIN {
    n = split("0.5 -0.5 1e-300 0.0 -0.0 0.02", v, " ")
    for (i = 1; i <= n; i++) {
        x = v[i] + 0
        r = ""
        if (x) r = r "if:T "; else r = r "if:F "
        r = r "?:" (x ? "T" : "F") " "
        r = r "&&:" (x && 1) " ||:" (x || 0) " !:" (!x) " "
        c = 0; while (x) { c++; break }; r = r "while:" c " "
        c = 0; for (; x;) { c++; break }; r = r "for:" c " "
        c = 0; do { c++ } while (x && c < 2); r = r "do:" c " "
        r = r "udf:" t(x)
        print v[i], r
    }
    nan = log(-1)
    if (nan) print "nan T"; else print "nan F"
    print "nan", (nan ? "T" : "F"), (nan && 1), (nan || 0)
    z = -0.0
    if (z) print "-0.0 T"; else print "-0.0 F"
    eps = 0.25; k = 0
    while (eps) { k++; eps = eps / 2; if (k >= 3) break }
    print "eps loop", k
}
$1 + 0 { print "pattern", NR, $1 }
$1 * 0.1 { print "scaled", NR }
