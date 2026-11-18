# stdin: data/para-blank.txt
# With RS = "", leading and trailing newlines are skipped; a line of spaces is not an empty line
# (gawk rswhite, leadnl, nlinstr).
BEGIN { RS = "" }
{ printf "%d <%s> NF=%d\n", NR, $0, NF }
END { print NR }
