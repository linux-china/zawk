# Assigning an empty string to a field keeps NF and rebuilds $0 with OFS (gawk fldchgnf).
BEGIN {
    $0 = "a b c"; OFS = ":"; $2 = ""; print $0; print NF
    $0 = "a b c"; $5 = ""; print $0; print NF
    $0 = "x y z"; NF = 0; print "[" $0 "]", NF
}
