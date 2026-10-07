function fill(arr, n,   i) { for (i = 1; i <= n; i++) arr[i] = i * i } BEGIN { fill(sq, 4); print sq[1], sq[2], sq[3], sq[4], length(sq) }
