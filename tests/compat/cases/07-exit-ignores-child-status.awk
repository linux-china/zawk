# exit: 3
# The exit status of a command read with getline does not affect awk's status (gawk exitval1).
BEGIN { "exit 1" | getline junk; print "[" junk "]"; exit 3 }
END { print "end" }
