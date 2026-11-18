# `$i++` increments the field, `$NF-1` is ($NF)-1, `-$1` negates a field (gawk prec, parsefld).
BEGIN {
    $0 = "3 5 9"
    i = 1
    print $i++, i, $1
    print $NF-1, $(NF-1), -$1, !$1, $1^2
    $NF++
    print $0
}
