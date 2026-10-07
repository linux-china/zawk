# stdin: data/spaces.txt
{ printf "%d:", NF; for (i = 1; i <= NF; i++) printf "[%s]", $i; print "" }
