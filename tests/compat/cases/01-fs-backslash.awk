# stdin: data/backslash.txt
# A single backslash as FS is a literal character, not an escape (gawk test fsbs).
BEGIN { FS = "\\" }
{ print NF; for (i = 1; i <= NF; i++) print i ": [" $i "]" }
