# stdin: data/a.txt
# "/dev/stdout" is the program's standard output: output keeps program order, and close()
# only flushes it.
{ print "1:" $0; print "2:" $0 > "/dev/stdout"; printf "3:%s\n", $0 > "/dev/stdout"; print "4:" $0 }
END { r = close("/dev/stdout"); print "closed", r; print "after" > "/dev/stdout" }
