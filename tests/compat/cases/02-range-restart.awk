# stdin: data/lines.txt
# A range restarts after its end; one that does not end runs to the last record; the end
# pattern is not tested on the starting record when they differ (gawk range1, range2).
/line1/, /line2/ { a = a NR }
/line2/, /line9/ { b = b NR }
/line[24]/, /line[24]/ { c = c NR }
NR % 2 == 1, NR % 2 == 1 { d = d NR }
END { print a, b, c, d }
