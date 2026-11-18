# stdin: data/emp.data
# A field passed to a function is copied: reassigning $0 inside does not change the parameter
# (gawk setrec0, setrec1).
function reassign(x, y) { $0 = x; print y, "|", $1 }
NR <= 2 { reassign("replaced record", $1) }
