# A global read in a function before the main program assigns it.
function show() { print "[" g "]"; if (flag) print "set"; else print "unset" }
BEGIN { show(); g = 5; flag = 1; show() }
