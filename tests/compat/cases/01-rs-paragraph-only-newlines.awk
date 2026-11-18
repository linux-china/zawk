# stdin: data/only-newlines.txt
# With RS = "", input made only of newlines has no record at all (gawk onlynl).
BEGIN { RS = "" }
{ print "got", $0 }
END { print "NR=" NR }
