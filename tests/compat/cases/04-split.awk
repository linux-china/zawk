BEGIN { n = split("a:b::c", arr, ":"); print n, arr[1], "[" arr[3] "]", arr[4]; n = split("  x  y ", w); print n, w[1], w[2]; n = split("", e); print n }
