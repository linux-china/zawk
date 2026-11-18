# stdin: data/longsub.txt
# A greedy `^.*` match in sub() takes the leftmost-longest match on a long record (gawk longsub).
{ x = $0; sub("^.*AA", "BB", x); print x; sub(/^A*/, "", $0); print length($0), $0 }
