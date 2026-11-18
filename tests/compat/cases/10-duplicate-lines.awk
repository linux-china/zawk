# stdin: data/repeats.txt
# Report lines seen before, with the line of their first occurrence (gawk numindex).
{ if ($0 in seen) { printf "line %d repeats line %d\n", NR, seen[$0]; dup++ } else seen[$0] = NR }
END { printf "%d %.2f%%\n", dup, dup / NR * 100 }
