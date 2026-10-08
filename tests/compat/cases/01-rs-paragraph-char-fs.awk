# stdin: data/para.txt
# Paragraph mode: with a single-character FS, newlines also separate fields.
BEGIN { RS = ""; FS = ":" }
{ printf "%d: NF=%d", NR, NF; for (i = 1; i <= NF; i++) printf " [%s]", $i; print "" }
