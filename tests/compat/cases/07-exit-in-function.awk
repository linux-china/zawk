# stdin: data/emp.data
# exit: 4
function check(x) { if (x > 20) exit 4; return x } { print check($3) } END { print "end" }
