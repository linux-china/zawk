# stdin: data/para.txt
# Paragraph mode: a regular expression FS does not split on newlines (as in gawk).
BEGIN { RS = ""; FS = ": *" }
{ printf "%d: NF=%d", NR, NF; for (i = 1; i <= NF; i++) printf " [%s]", $i; print "" }
