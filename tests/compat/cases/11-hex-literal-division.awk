# `/` after a hex literal is division, not the start of a regex
BEGIN {
    print 0x10 / 2 / 1
    x = 0x10 / 4; print x
    print 0x10/2/2
}
