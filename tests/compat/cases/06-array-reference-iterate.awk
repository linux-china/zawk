# stdin: data/emp.data
# Elements created only by referencing `a[k]` keep their keys when iterated.
{ seen[$1] }
END {
    for (k in seen) { n++; if (k == "") empty++; if (k in seen) found++ }
    print n, empty + 0, found
    for (i = 0; i < 5; i++) a[i]
    for (k in a) sum += k
    for (k in a) delete a[k]
    print sum, length(a)
}
