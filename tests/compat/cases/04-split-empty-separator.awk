# stdin: data/utf8.txt
# An empty separator (FS = "" or split(s, a, "")) splits into characters, including multi-byte
# UTF-8 characters; split(s, a) uses FS.
BEGIN { n = split("abc", a, ""); print n, a[1], a[3]; sep = ""; print split("xyz", b, sep), b[2] }
NR == 1 { FS = "" }
NR == 2 { printf "%d:", NF; for (i = 1; i <= NF; i++) printf " [%s]", $i; print ""; print split($0, c), c[1] }
