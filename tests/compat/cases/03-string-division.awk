# Arithmetic on string constants converts them; no division-by-zero error for "2"/"3" (gawk
# divzero2). Modulus follows the sign of the dividend.
BEGIN {
    print "2" / "3", "10" * "0.5", "7" % "4"
    print 7.5 % 2, -7 % 3, 7 % -3, -7.5 % 2
}
