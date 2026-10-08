# stdin: data/strnum.txt
# Fields (and copies of them) that look numeric compare as numbers; others compare as strings.
{
    print ($1 > $2), ($1 == $3), ($5 == 10), ($6 == 7), ($7 > 9), ($4 > 5)
    x = $1; y = $2; a[1] = $1; a[2] = $2
    print (x > y), (a[1] > a[2]), (" 10 " > 9)
    n = split($0, p); print (p[1] > p[2])
    max = $1; for (i = 2; i <= 3; i++) if ($i > max) max = $i; print max
}
