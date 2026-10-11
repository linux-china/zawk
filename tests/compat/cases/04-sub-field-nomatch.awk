# Writing back to the target of sub()/gsub() only happens when something matched: no rebuild
# of $0 with OFS, no new fields on empty lines, array elements keep their value.
BEGIN {
  OFS = "-"
  r[1] = "a b c"; r[2] = ""; r[3] = "  x   y  "
  for (i = 1; i <= 3; i++) {
    $0 = r[i]
    n = sub(/zz/, "y", $2)
    print n, "[" $0 "]"
    n = gsub(/b|y/, "Q", $2)
    print n, "[" $0 "]"
  }
  a["k"] = "abc"
  print sub(/z/, "", a["k"]), a["k"], sub(/z/, "", a["m"]), ("m" in a), length(a)
}
