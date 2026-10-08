# Awk regex syntax: `.` matches newlines, `{` that is not an interval is literal, gawk's word and
# buffer operators, bracket expressions with `[`, `]` and `&`, and octal escapes.
BEGIN {
    s = "a\nb"; print (s ~ /a.b/), (s ~ "a.b"), (s ~ /^a$/)
    print ("{" ~ /{/), ("a{" ~ /a{/), ("a{,}" ~ /a{,}/), ("aa" ~ /^a{2}$/), ("a" ~ /^a{2}$/)
    print ("foo bar" ~ /\yfoo\y/), ("afoo" ~ /\yfoo/), ("foo" ~ /\<foo\>/), ("ab" ~ /\`ab/)
    print ("[" ~ /[[]/), ("]" ~ /[]]/), ("&" ~ /[a&]/), ("x" ~ /[^]a]/), ("A" ~ /\101/)
    t = "hello world"; gsub(/\<./, "X", t); print t
}
