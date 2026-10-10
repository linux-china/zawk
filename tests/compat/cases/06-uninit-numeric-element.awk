# A missing element of an array of numbers reads as "" where a string is expected.
BEGIN {
    a[1] = 5; c["x"] += 2
    print "[" a[2] "]", length(c["zz"])
    print c["x"], c["y"]
    print (a[1] == "5")
}
