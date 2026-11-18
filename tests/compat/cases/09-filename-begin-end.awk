# args: data/a.txt data/b.txt
# FILENAME is empty in BEGIN and keeps the last file name in END.
BEGIN { print "[" FILENAME "]" }
FNR == 1 { print FILENAME, NR }
END { print FILENAME, NR, FNR }
