# Deleting every element of an array parameter inside `for (k in a)`, `delete` of a whole array
# parameter, and length() of an array parameter (gawk fnparydl, fnarydel, funlen).
function delit(a,    k, n) {
    for (k in a) { delete a[k]; n++ }
    return n
}
function wipe(a) { delete a }
function size(arr) { return length(arr) }
BEGIN {
    for (i = 1; i <= 7; i++) { q[i] = sprintf("element %d", i); x[i] = i }
    print delit(q), "deleted,", length(q), "elements still in q[]"
    print size(x); wipe(x); print size(x)
    x["again"] = 1; print size(x), ("again" in x)
    print size(never)
}
