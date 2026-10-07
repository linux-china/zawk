BEGIN { split("a a b b b c a", w, " "); for (i = 1; i in w; i++) if (w[i] != prev) { printf "%s ", w[i]; prev = w[i] } print "" }
