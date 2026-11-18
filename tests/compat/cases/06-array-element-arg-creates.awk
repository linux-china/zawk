# Passing a missing element to a function, reading it, or comparing it creates the element with
# an empty value; the element stays false (gawk elemnew1, elemnew2, elemnew3).
function id(x) { return x }
function len(s) { return length(s) }
BEGIN {
    print ("b" in a)
    id(a["b"]); print ("b" in a), length(a), (a["b"] ? "true" : "false"), "[" a["b"] "]"
    x = c["k"]; print ("k" in c)
    if (d["z"] == "") print ("z" in d)
    print len(e["t"]), ("t" in e)
    e["t"] = e["t"]; print length(e)
}
