# Bracket ranges in the C/UTF-8 locale only include the listed letters (gawk regexprange, rri1).
BEGIN {
    range = "[a-dx-z]"
    s = ""
    for (c = 65; c <= 90; c++) { ch = sprintf("%c", c); if (ch ~ range) s = s ch }
    for (c = 97; c <= 122; c++) { ch = sprintf("%c", c); if (ch ~ range) s = s ch }
    print "[" s "]"
    print ("e" ~ /[d-f]/), ("E" ~ /[d-f]/), ("-" ~ /[a\-z]/), ("b" ~ /[^a-c]/)
}
