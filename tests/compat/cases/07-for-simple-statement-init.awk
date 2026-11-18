# The parts of a `for` header are simple statements, including print (gawk forsimp).
BEGIN { for (print 9; 0;) ; for (i = 0; i < 2; print "step", i++) ; print "done" }
