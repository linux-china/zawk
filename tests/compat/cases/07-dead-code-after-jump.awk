# stdin: data/a.txt
# Statements after break, continue, next, nextfile and return never run, but must compile.
function f() { return 1; print "after return" }
function g(x) { if (x) { return "y"; x = 5 } else { return "n"; x = 6 } print "dead" }
FNR == 1 { print "first:", $0; next; print "after next" }
{ while (1) { break; print "after break" } }
{ for (i = 0; i < 3; i++) { continue; print "after continue" } }
{ print "line", FNR, f(), g(1), g(0); nextfile; print "after nextfile" }
END { print NR }
