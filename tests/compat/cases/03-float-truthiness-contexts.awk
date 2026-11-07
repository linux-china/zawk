# stdin: data/float-ratio.txt
# More float truth contexts: NaN in loops and `!`, compound pattern expressions, range patterns,
# array elements, compound assignments and ratios computed from fields.
BEGIN {
    nan = log(-1)
    c = 0; while (nan) { c++; break }; print "nan while", c
    c = 0; for (; nan;) { c++; break }; print "nan for", c
    c = 0; do { c++ } while (nan && c < 3); print "nan do", c
    print "nan !", (!nan), (!!nan)
    z = -0.0
    print "-0.0", (z && 1), (z || 0), (!z), (z ? "T" : "F")
    a["half"] = 0.5; a["zero"] = 0.0; a["tiny"] = 1e-10
    for (k in a) out[k] = (a[k] ? "T" : "F")
    print "array", out["half"], out["zero"], out["tiny"]
    x = 1; x *= 0.5; print "*=", (x ? "T" : "F")
    x = 1; x /= 4; print "/=", (x ? "T" : "F")
    x = 0.5; x -= 0.5; print "-=", (x ? "T" : "F")
    r = 1 / 3; print "1/3", (r ? "T" : "F"), (r && r) , (0 || r)
    print "nested", (0.5 ? (0.25 ? "TT" : "TF") : "F")
}
$3 / ($2 + 10) { print "ratio", $1 }
$2 && $3 / $2 { print "guarded", $1 }
!($3 / ($2 + 1)) { print "not-ratio", $1 }
($3 / 10) || ($2 / 100) { print "or", $1 }
($3 / 10 ? 1 : 0) { print "ternary", $1 }
$3 / 10, !($3 / 10) { print "range", $1 }
