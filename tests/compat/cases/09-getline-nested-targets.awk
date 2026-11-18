# The temporaries of a getline are per call: a getline with an indexed target in the redirection
# of another one, or in a recursive call made from it, keeps its own subscript, and a redirection
# with side effects is evaluated once.
function src(depth) {
    if (depth > 0)
        getline b[++k] < src(depth - 1)
    return "data/a.txt"
}
BEGIN {
    # Indexed getline inside the redirection of an indexed getline.
    getline a[++i] < ((getline b[++j] < "data/b.txt") > 0 ? "data/a.txt" : "none")
    print i, j, "[" a[1] "]", "[" b[1] "]", length(a), length(b)
    close("data/a.txt"); close("data/b.txt")

    # Recursion from the redirection: each level reads into its own element.
    k = 0; delete b
    getline c[++m] < src(2)
    print k, m, "[" b[1] "]", "[" b[2] "]", "[" c[1] "]", length(b), length(c)
    close("data/a.txt")

    # The redirection runs once per call.
    split("data/a.txt data/b.txt", files, " ")
    n = 0
    while (n < 2 && (getline line < files[++n]) > 0) print n, line
    print "n =", n
}
