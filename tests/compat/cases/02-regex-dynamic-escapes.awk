# A string used as a regex goes through string escapes first: "\\." is a literal dot,
# "\." is a dot as well (with a warning in gawk), "." matches any character.
BEGIN {
    s = "a.b"; t = "axb"
    print (s ~ "a\\.b"), (t ~ "a\\.b"), (t ~ "a.b"), (s ~ /a\.b/), (t ~ /a\.b/)
    re = "^[0-9]+$"
    print ("123" ~ re), ("12a" ~ re), ("" ~ re)
    re = "b+"; x = "abbbc"; gsub(re, "-", x); print x
}
