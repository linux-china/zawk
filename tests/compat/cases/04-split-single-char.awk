# A single-character separator (other than " ") is a literal character, in split() and in FS
# assigned at runtime; a single-character regex literal stays a regular expression.
BEGIN {
    print split("a.b.c", a, "."), split("a|b|c", a, "|"), split("a^b^c", a, "^"), split("a\\b", a, "\\")
    sep = "."; print split("x.y", a, sep), split("a.b.c", a, /./), split("a,b,,c", a, ","), "[" a[3] "]"
    FS = "."; $0 = "p.q.r"; print NF, $2
    FS = "|"; $0 = "p|q"; print NF, $2
}
