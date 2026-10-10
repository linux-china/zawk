# stdin: data/a.txt
# An uninitialized variable is both 0 and "", even when it is only ever assigned numbers.
FNR == 1 { print "[" cnt "]", length(cnt) }
{ if (cnt == "") print "first"; cnt++ }
END {
    print "[" cnt "]"
    y = never
    print "[" y "]", (y == ""), (y == 0)
    OFMT = "%.2f"; print f; f = 3.14159; print f
}
