# The predefined PROCINFO["sorted_in"] orders (gawk sortfor, arraysort).
function walk(order,    k, s) {
    PROCINFO["sorted_in"] = order
    s = ""
    for (k in a) s = s " " k "=" a[k]
    print order ":" s
}
BEGIN {
    a["b"] = 3; a["a"] = 10; a[10] = "x"; a[9] = 2; a["c"] = "y"
    walk("@ind_str_asc"); walk("@ind_str_desc")
    walk("@ind_num_asc"); walk("@ind_num_desc")
    walk("@val_num_asc"); walk("@val_num_desc")
    walk("@val_str_asc"); walk("@val_str_desc")
    walk("@val_type_asc"); walk("@val_type_desc")
}
