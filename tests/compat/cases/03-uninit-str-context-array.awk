# stdin: data/a.txt
# A missing element of an array only assigned numbers is "" in every string context, not "0".
{ if ($1 == "no such field") c[$2]++; n[NR] += NF }
END {
    if (c[2] ~ /^$/) print "missing"
    print (n["nope"] ~ "0"), (n["nope1"] !~ /^$/)
    printf "%s=%s\n", "nope", n["nope2"]
    x = sprintf("[%s]", n["nope3"]); print x
    print toupper(n[100]) "|" substr(n[101], 1) "|" index(n[102], "0")
    b[n[103]] = 1
    for (k in b) print "[" k "]"
    print (n[1] ~ /^[0-9]+$/), toupper(n[1]), n[1] "" == "2"
}
