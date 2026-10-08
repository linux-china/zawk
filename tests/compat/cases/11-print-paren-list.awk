# A parenthesized argument list after `print ` or `printf ` (with a space), possibly followed by
# a redirection; a parenthesized single expression or `(a, b) in arr` is not an argument list.
BEGIN {
    print (1, 2)
    printf ("%d-%s\n", 5, "x")
    print (1, 2) | "cat"; close("cat")
    print (1)(2), (1) == 1
    a[1, 2]; print (1, 2) in a
    x = 6; print (x / 2, x / 3, "a,b", f(1, 2))
}
function f(p, q) { return p + q }
