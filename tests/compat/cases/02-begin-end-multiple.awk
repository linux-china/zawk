# stdin: data/emp.data
BEGIN { printf "a" } BEGIN { printf "b\n" } END { print NR } END { print "done" }
