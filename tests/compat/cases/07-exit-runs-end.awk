# stdin: data/emp.data
# exit: 3
NR == 3 { exit 3 } { print $1 } END { print "end", NR }
