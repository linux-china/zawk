# A regex RS that can match the empty string must not drop the input: when it has no non-empty
# match the whole input is one record, otherwise the non-empty matches separate the records.
# (gawk's results for empty matches in the middle of a record, e.g. RS="b?", are not
# checked: gawk consumes characters there.)
function show(rs,    f, line, n) {
  f = "data/rs-empty-match.txt"
  RS = rs
  while ((getline line < f) > 0)
    printf "%s %d [%s]\n", rs, ++n, line
  close(f)
}
BEGIN { show("q*"); show("x*"); show("(q|r)?") }
