# stdin: data/emp.data
BEGIN { print "begin"; exit } { print "never" } END { print "end", NR }
