# ++, -- and += on array elements that hold strings: counters and labels in one array, the
# `arr[++arr[0]] = v` list idiom, and counters started from field values.
# stdin: data/emp.data
function push(arr, v) { arr[++arr[0]] = v }
{ cnt[$1]++; cnt["label"] = "employees"; hours[$1] = $3; hours[$1] += 10 }
END {
    print cnt["Beth"], cnt["label"], hours["Beth"], hours["Mark"]
    push(list, "a"); push(list, "b"); print list[0], list[1], list[2]
    s[++s[0]] = "x"; s[++s[0]] = "y"; print s[0], s[1], s[2]
    a["k"] = "v"; a["k"]--; print a["k"]
    a["n"] = "1.5"; a["n"]++; print a["n"]
    b["k"] = "v"; x = b["k"]++; y = ++b["k"]; print x, y, b["k"]
    c[1] = "x"; c[1] += 2; c[2]++; print c[1], c[2]
    v = "abc"; w = v++; print w, v
}
