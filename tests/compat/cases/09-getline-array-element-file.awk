# The target of `getline var < file` can be an array element whose subscript has a side effect;
# it is evaluated once per call, also for the call that hits end of file (gawk getline5).
BEGIN {
    file = "data/three-lines.txt"
    while ((getline a[++c] < file) > 0) {}
    print c, a[1], a[3], "[" a[4] "]", length(a)
    close(file)
    n = 0
    while ((getline line < file) > 0) n++
    print n, line
}
