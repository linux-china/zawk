BEGIN { i = 0; while (i < 3) printf "%d ", i++; do { printf "d%d ", i } while (--i > 0); for (j = 0; j < 3; j++) printf "f%d ", j; print "" }
