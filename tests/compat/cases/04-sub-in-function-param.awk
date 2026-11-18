# gsub on a local parameter does not change the caller's variable (gawk manglprm).
function protect(s) { gsub("\n", "\\n", s); return s }
BEGIN {
    buf = "line1\nline2"
    print "[" protect(buf) "]"
    n = split(buf, parts, "\n"); print n, parts[2]
}
