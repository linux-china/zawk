# strftime() formats each conversion on its own (an unknown one does not replace the whole
# format), supports the E/O modifiers, and gives an empty string for an out-of-range timestamp.
BEGIN {
    t = 1621530000
    print strftime("%Y-%m-%d %H:%M:%S", t, 1)
    print strftime("[%Y] %j %a %b %e", t, 1)
    print strftime("%Ey/%Om/%Od", t, 1)
    print "[" strftime("%Y", 2^62, 1) "]"
    print "[" strftime("", t, 1) "]"
}
