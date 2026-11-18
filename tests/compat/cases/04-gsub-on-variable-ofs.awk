# sub/gsub can target any lvalue: a special variable, an array element, a number, a field
# (gawk gsubtst8, gsubasgn).
BEGIN {
    $0 = "a foo b"
    OFS = " " $2 " "; gsub("foo", "_", OFS); print $1, $3
    x = 12321; n = gsub(/2/, "X", x); print n, x, length(x)
    a["k"] = "aaa"; sub(/a/, "b", a["k"]); print a["k"]
    gsub(/o/, "0", $2); print; print NF
    s = "x"; print sub(/y/, "z", s), s
}
