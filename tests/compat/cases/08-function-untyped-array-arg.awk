function first(a,  k) { for (k in a) return k; return "none" }
function cnt(a,  k, n) { for (k in a) n++; return n }
BEGIN { x["p"]; print first(x); print first(empty); print cnt(x); print cnt(empty2) + 0 }
