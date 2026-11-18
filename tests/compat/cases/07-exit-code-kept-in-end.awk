# exit: 12
# `exit` without a value in END keeps the status of an earlier `exit n` (gawk exit2, exitval3).
BEGIN { print "begin"; exit 12 }
END { print "end"; exit }
