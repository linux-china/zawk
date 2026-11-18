# A regex FS made of tabs: runs of tabs form one separator, spaces stay in the fields (gawk fstabplus).
BEGIN {
    FS = "\t+"
    $0 = "a b\t\t\tc\td  e"; print NF; for (i = 1; i <= NF; i++) print "[" $i "]"
    $0 = "\tlead"; print NF, "[" $1 "]", "[" $2 "]"
}
