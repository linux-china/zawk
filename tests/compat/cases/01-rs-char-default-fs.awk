# stdin: data/rs-semicolon.txt
# With a single-character RS, the default FS still splits on runs of blanks and newlines.
BEGIN { RS = ";" }
{ printf "%d:", NF; for (i = 1; i <= NF; i++) printf " [%s]", $i; print "" }
