BEGIN { n = split("a1b22c333d", arr, /[0-9]+/); for (i = 1; i <= n; i++) printf "%s ", arr[i]; print n }
