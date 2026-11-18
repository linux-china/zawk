# args: data/a.txt data/b.txt data/lines.txt
# Deleting or emptying an ARGV element skips that operand; adding one and raising ARGC reads it
# (gawk delargv, argarray).
BEGIN {
    delete ARGV[1]
    ARGV[2] = ""
    ARGV[ARGC++] = "data/a.txt"
}
FNR == 1 { print FILENAME }
END { print NR, ARGC; for (i = 1; i < ARGC; i++) if (i in ARGV) print i, length(ARGV[i]) }
