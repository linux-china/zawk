# exit: 1
# The right operand of `in` is an array name, not a concatenation.
BEGIN { a[1]; print 1 in a "x" }
