BEGIN { a[1] = "c"; a[2] = "a"; a[3] = "b"; n = asort(a); for (i = 1; i <= n; i++) printf "%s ", a[i]; print n }
