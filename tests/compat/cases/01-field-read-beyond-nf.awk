# Reading a field past NF yields "" and does not change NF or $0.
BEGIN {
    $0 = "a b"
    x = $5
    print "[" x "]", NF, "[" $0 "]"
    if ($7 == "") print "empty"
    print NF
}
