# A string converts to the number made by its longest numeric prefix (gawk hex, leaddig).
BEGIN {
    print "1(e)" + 0, "3e2(ex)" + 0, "6e5(x)" + 0, "2E" + 0, "2E+" + 0, ".5." + 0, "+-3" + 0
    print "  12abc" + 0, "-.25x" + 0, "1e" + 0, "e5" + 0, "--1" + 0, "1.5e-1z" + 0
    x = "2E"; print (x == 2), (x == "2E")
    print 010e2, 1.e1, .5e1
}
