# Operands are evaluated from left to right: a variable operand has its value from before the
# side effects of the operands after it.
function bump() { g = 100; return 1 }
BEGIN {
    i = 5; print i++, i, ++i
    i = 5; print i, i++
    i = 5; x = i + i++; print x
    i = 5; x = i (i = 9); print x
    i = 5; y = i * (i = 2); print y
    i = 5; print i, (i += 2), i
    i = 5; printf "%d %d\n", i, i++
    i = 5; s = sprintf("%d-%d", i, i++); print s
    g = 5; print g, bump(), g
    s = "aaa"; print s, gsub(/a/, "b", s), s
}
