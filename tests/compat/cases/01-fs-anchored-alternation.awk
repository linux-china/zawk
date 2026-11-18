# stdin: data/uparrfs.txt
# `^` in an alternative of a regex FS only matches at the start of the record (gawk uparrfs).
BEGIN { FS = "(^x+)|( +)" }
{
    for (i = 1; i <= NF; i++) printf "-->%s<--\n", $i
    print "NF=" NF
}
