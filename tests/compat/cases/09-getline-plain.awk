# stdin: data/emp.data
NR == 1 { getline; print "after getline:", $1, NR } END { print NR }
