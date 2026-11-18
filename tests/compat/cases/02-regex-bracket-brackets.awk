# Brackets inside bracket expressions: `[]]`, `[][]`, `[\[]`, `[[]`, `[^]]` (gawk rebrackloc,
# regexpbrack).
BEGIN {
    n = split("[ ] x [] a]b", s, " ")
    for (i = 1; i <= n; i++)
        printf "%-4s %d %d %d %d %d\n", s[i], (s[i] ~ /[]]/), (s[i] ~ /[][]/), (s[i] ~ /[\[]/), (s[i] ~ /[[]/), (s[i] ~ /^[^]]+$/)
    if (match("New Value [XYZ]", /([Nn]ew) Value +[\([]? *([[:upper:]]+)/))
        print RSTART, RLENGTH, substr("New Value [XYZ]", RSTART, RLENGTH)
    x = "a[b]c"; gsub(/[][]/, "|", x); print x
}
