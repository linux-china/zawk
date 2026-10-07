BEGIN { f = "/dev/stdout"; print "to stdout" > f; printf "%s\n", "again" > f }
