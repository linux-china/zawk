# Reading a missing file is not fatal: getline returns -1, and the program continues.
BEGIN {
    while ((getline line < "data/no-such-file") > 0)
        print "unexpected:", line
    if ((getline line < "data/no-such-file") < 0)
        print "missing"
    var = "data/no-such-file"
    print (getline x < var), (getline x < var)
    print "after"
}
