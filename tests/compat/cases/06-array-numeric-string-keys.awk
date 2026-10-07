BEGIN { a[1] = "one"; print a["1"]; a["01"] = "zero-one"; print a[1], a["01"]; x = 0.1 + 0.2; a[x] = "f"; print length(a) }
