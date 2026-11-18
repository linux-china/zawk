# `delete arr` empties an array, which can then be reused; deleting inside a for-in loop over an
# empty array, and deleting missing elements, are harmless (gawk aadelete1, fordel, delarprm).
function clear(arr) { delete arr }
BEGIN {
    for (i in none) delete none
    a[1]; a[2]; a["x"] = 3
    delete a; print length(a), (1 in a)
    a["y"] = 5; print length(a), a["y"]
    delete a["missing"]; print length(a)
    split("p q r", b); clear(b); print length(b)
    b[9] = 1; print length(b)
}
