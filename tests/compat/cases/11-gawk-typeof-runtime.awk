# stdin: data/typeof.txt
# opts: -v vv=7
# typeof reports the state at runtime: input strings that look numeric are "strnum", missing
# array elements are "untyped", fields beyond NF are "unassigned", and a variable is "untyped"
# until it is assigned. $0 is "unassigned" until a record is read or it is set.
function late() { return typeof(g) }
BEGIN {
    print late(), typeof(vv)
    print typeof($0), typeof($1); $0 = "x"; print typeof($0)
    g = 1
    print late()
    split("1 a", parts)
    print typeof(parts[1]), typeof(parts[2]), typeof(parts[3]), length(parts)
    a[1] = 1
    print typeof(a[1]), typeof(a[2]), length(a)
    s = "1"; t = s ""
    print typeof(s), typeof(t)
    m["x"]; print typeof(k); for (k in m) ; print typeof(k)
    v = "ab"; sub(/a/, "b", v); print typeof(v)
}
{
    print typeof($0), typeof($1), typeof($2), typeof($3), typeof($4), typeof($(NF + 1))
    if (NF) { x = $1; print typeof(x) }
}
END {
    getline line < "data/typeof.txt"
    print typeof(line)
}
