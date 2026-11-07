# A numeric variable that is never assigned is "" in every string context, not "0".
BEGIN {
    if (0) u++
    print (u ~ /^$/), (u ~ "0"), (u !~ /^$/)
    printf "%s|%d|%5s|%.2f\n", u, u, u, u
    x = sprintf("%s=%s", "u", u); print x
    print toupper(u) "|" substr(u, 1) "|" index(u, "0") "|" split(u, parts)
    t = "a"; sub(/a/, u, t); print "[" t "]"
    u = 3
    print (u ~ /^3$/), toupper(u), substr(u, 1), index(u, "3")
    printf "%s %d\n", u, u
}
