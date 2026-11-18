# stdin: data/lines.txt
# A getline loop in BEGIN consumes standard input, so the main rules see no records (gawk
# getline2).
BEGIN { while ((getline) > 0) n++; print n, NR, $0 }
{ print "main", $0 }
END { print "end", NR }
