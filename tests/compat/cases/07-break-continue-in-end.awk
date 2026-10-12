# break and continue in loops of BEGIN and END, in a program that has main rules.
# stdin: data/a.txt
BEGIN { for (i = 0; i < 5; i++) { if (i == 1) continue; if (i == 3) break; print "begin", i } }
{ n++ }
END {
    for (i = 0; i < 5; i++) { if (i == 1) continue; if (i == 3) break; print "end", i }
    while (1) { if (++k > 2) break }
    print n, k
}
