# stdin: data/three-lines.txt
# Escaped braces and the gawk shorthands \w, \s, \S in regex literals and in dynamic regexes
# (gawk escapebrace, backw, backbigs1, backsmalls1).
/\w+ [2]/ { print "w:", $0 }
BEGIN {
    print ("foo{bar" ~ /\{/), ("foo}bar" ~ /\}/), ("a{2}" ~ /a\{2\}/), ("aa" ~ /^a{2}$/)
    print (" " ~ /^\s$/), ("\t" ~ /^\s$/), ("x" ~ /^\s$/), ("x" ~ /\S/), ("" ~ /\S/)
    print ("a_1" ~ /^\w+$/), ("a-1" ~ /^\w+$/), ("{" ~ "\\{"), ("a b" ~ "a\\sb")
}
