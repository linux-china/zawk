# stdin: data/matrix.txt
# Transpose a matrix.
{ for (i = 1; i <= NF; i++) m[NR, i] = $i; if (NF > cols) cols = NF }
END { for (i = 1; i <= cols; i++) { s = m[1, i]; for (j = 2; j <= NR; j++) s = s " " m[j, i]; print s } }
