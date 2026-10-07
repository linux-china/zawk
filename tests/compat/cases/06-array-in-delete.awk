BEGIN { a[1]; a[2] = ""; a["x"] = 3; delete a[2]; print (1 in a), (2 in a), ("x" in a), length(a); delete a; print length(a) }
