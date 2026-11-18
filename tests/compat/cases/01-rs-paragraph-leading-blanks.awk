# stdin: data/rs-para-leading.txt
# With RS = "", blanks around the record text are not part of the separator, but fields are
# still split on blanks and newlines (gawk rsnulw).
BEGIN { RS = "" }
{
    print NR, NF, "<" $0 ">"
    for (i = 1; i <= NF; i++) print i, "[" $i "]"
}
