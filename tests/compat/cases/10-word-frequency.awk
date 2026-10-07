# stdin: data/b.txt
{ for (i = 1; i <= NF; i++) w[$i]++ } END { print w["first"], w["third"] }
