# stdin: data/nfset.txt
# Assigning NF pads the record with empty fields or drops the trailing ones, and $0 is rebuilt
# with OFS (gawk nfset, nfloop, fldchgnf).
{ NF = 5; print NR ": [" $0 "]", NF }
END {
    $0 = "aaa"; NF = 6
    for (j = 2; j <= NF; ++j) $j = "_"
    print
    OFS = "-"; NF = 3; print; print NF, $3, "[" $4 "]"
}
