# Multi-dimensional subscripts join with the value of SUBSEP at the time of the access (gawk
# subsepnm).
BEGIN {
    SUBSEP = ":"
    a[1, 2] = "x"; a["p", "q", "r"] = "y"
    PROCINFO["sorted_in"] = "@ind_str_asc"
    for (k in a) print k, a[k]
    print (("1", 2) in a), ("1:2" in a)
    SUBSEP = "\034"
    print ((1, 2) in a)
}
