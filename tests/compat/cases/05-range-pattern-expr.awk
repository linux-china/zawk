# stdin: data/emp.data
# Range patterns whose ends are full expressions, with and without actions.
NR == 2, NR == 3
NR == 4, $3 == 0 { print "to-zero:", $1 }
$2 > 4.5 && NR > 1, NR % 3 == 0 { print "mixed:", NR }
NR == 1,
    NR == 1 { print "single:", $1 }
