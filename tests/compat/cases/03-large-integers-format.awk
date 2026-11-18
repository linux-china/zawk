# Large integral values print as integers with print, %s and %d, and %g/%o still work
# (gawk double1, double2, ofmtbig).
BEGIN {
    x = 2 ^ 50
    for (i = 50; i <= 62; i += 4) {
        printf "2^%d= %s %d %g %o %x\n", i, x, x, x, x, x
        x *= 16
    }
    OFMT = "%.12g"; big = 99999999999; print big, big + 1, big / 10, -big
    printf "%d %d %i\n", 1e15, -1e15, 2^53 + 2
}
