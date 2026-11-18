# stdin: data/three-lines.txt
# A multi-character RS that never occurs makes the whole input one record, and the final newline
# is part of it (gawk rstest6).
BEGIN { RS = "XYZ" }
{ printf "%d [%s] NF=%d\n", NR, $0, NF }
