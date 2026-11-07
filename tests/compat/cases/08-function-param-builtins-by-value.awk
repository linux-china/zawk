# Scalar parameters are passed by value: builtins that write their argument in place (sub, gsub,
# getline var, split's target) only change the callee's copy, for string and number arguments
# from globals, locals and fields. Array parameters are passed by reference.
# stdin: data/a.txt
function s_sub(s) { sub(/a/, "X", s); return s }
function s_gsub(s) { n = gsub(/[0-9]/, "#", s); return n ":" s }
function s_gensub(s) { s = gensub(/(.)/, "<\\1>", "g", s); return s }
function s_getline(s) { getline s < "data/a.txt"; close("data/a.txt"); return s }
function s_split(s,    parts) { n = split(s, parts, "a"); s = parts[1]; return n ":" s }
function s_all(s) { sub(/./, "1", s); gsub(/./, "2", s); getline s < "data/a.txt"; close("data/a.txt"); return s }
function n_sub(x) { sub(/2/, "9", x); x++; return x }
function n_gsub(x) { gsub(/./, "7", x); return x + 0 }
function a_split(arr, s) { return split(s, arr, ",") }
function a_sub(arr) { sub(/a/, "X", arr["k"]); gsub(/b/, "Y", arr["k"]) }
function nested(s) { s_sub(s); s_all(s); sub(/a/, "Z", s); return s }
function locals(    t, u) {
    t = "banana"; u = 123
    print s_sub(t), s_gsub(u), s_gensub(t), s_getline(t), s_split(t), s_all(u)
    print t, u
}
BEGIN {
    g = "aaa"; num = 123; frac = 2.5
    print s_sub(g), s_gsub(num), s_gensub(g), s_getline(g), s_split(g), s_all(g), nested(g)
    print g, num
    print n_sub(num), n_gsub(num), n_sub(frac), num + 1, frac * 2
    locals()
    a["k"] = "abab"; split("", b)
    print a_split(b, "x,y,z"), length(b), b[3]
    a_sub(a); print a["k"]
}
NR == 1 {
    print s_sub($0), s_gsub($1), s_getline($1), s_all($0)
    print $0, $1, NF
    line = $0; f1 = $1
    print s_sub(line), s_gsub(f1), nested(line)
    print line, f1
}
