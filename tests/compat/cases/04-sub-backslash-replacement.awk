# Backslashes and `&` in the replacement of sub/gsub (gawk subback, backgsub, subamp).
BEGIN {
    r[1] = "&"; r[2] = "\\&"; r[3] = "\\\\&"; r[4] = "\\\\\\&"; r[5] = "\\q"; r[6] = "a\\\\b"
    r[7] = "[&&]"; r[8] = "\\\\"
    for (i = 1; i <= 8; i++) { x = "aBc"; sub(/B/, r[i], x); printf "%d %s %s\n", i, r[i], x }
    s = "a\\b\\c"; n = gsub(/\\/, "/", s); print n, s
    s = "a\\b"; gsub("\\\\", "\\\\", s); print s
    s = "abc"; sub(/[[:lower:]]/, "&", s); print s
}
