# gensub(): "g"/"G", a numeric occurrence, \0 and &, escaped &, and $0 as the default target
# (gawk gensub, gensub2, gensub3).
BEGIN {
    s = "hello world"
    print gensub(/o/, "0", "g", s), gensub(/o/, "0", "G", s), gensub(/o/, "0", 1, s), gensub(/o/, "0", 2, s)
    print gensub(/(l+)/, "<\\1>", "g", s), gensub(/w.*/, "[&]", 1, s), gensub(/w.*/, "[\\0]", 1, s)
    print gensub(/o/, "\\&", "g", s), s
    $0 = "a-b-c"; print gensub(/-/, "+", "g"), $0
    print gensub(/x/, "y", "g", "none"), gensub(/(a)(b)?/, "[\\2]", "g", "ab a")
}
