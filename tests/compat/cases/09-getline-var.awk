# stdin: data/emp.data
NR == 1 { getline x; print "x=" x, "$0=" $0, NR }
