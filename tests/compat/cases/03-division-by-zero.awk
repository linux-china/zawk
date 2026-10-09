# exit: 2
# Division by zero is fatal, as `%` by zero; output printed before the error is kept.
BEGIN {
    print 7 / 2, -7 / 2, 0 / 5
    x = "0"
    print "before"
    print 1 / x
    print "not reached"
}
