# An octal escape in a regex stands for that literal character, even a metacharacter (gawk litoct).
BEGIN {
    print ("a*b" ~ /a\52b/), ("aab" ~ /a\52b/), ("a+b" ~ "a\\53b"), ("A" ~ /^\101$/)
}
