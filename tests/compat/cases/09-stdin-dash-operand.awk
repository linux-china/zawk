# stdin: data/lines.txt
# args: data/a.txt - data/b.txt
# A `-` operand reads standard input in its place.
FNR == 1 { print "file:", FILENAME }
{ n[FILENAME]++ }
END { print n["data/a.txt"], n["-"] + n[""], n["data/b.txt"] }
