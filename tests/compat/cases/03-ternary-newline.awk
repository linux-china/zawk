# A newline may follow `?` and `:` in a conditional expression.
function sign(n) {
    return n > 0 ?
        "pos" :
        n < 0 ? "neg" :
            "zero"
}
BEGIN {
    x = 1 ?
        "a" : "b"
    print x
    print sign(3), sign(-2), sign(0)
    for (i = (0 ?
              5 :
              1); i <= 2; i++)
        print "i", i
    y = 0 ? 1 :
        2
    print y
}
