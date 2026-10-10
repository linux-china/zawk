# stdin: data/nan-inf-words.txt
# Words starting with "nan" or "inf" convert to 0; only signed "+inf", "-inf", "+nan", "-nan"
# (nothing but blanks after them) are the special values.
{ v = $0 + 0; n++; print n, (v == v ? v : "nan"); if (v == v && v < 1e300 && v > -1e300) s += v }
END { print "sum", s; print "strtonum", strtonum("info"), strtonum("Nancy") }
