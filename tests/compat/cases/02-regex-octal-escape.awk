# An octal escape of a metacharacter in a regex is that operator (`\52` is `*`), as in gawk (litoct).
BEGIN {
    print ("a*b" ~ /a\52b/), ("aab" ~ /a\52b/), ("a+b" ~ "a\\53b"), ("A" ~ /^\101$/)
}
