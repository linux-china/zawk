BEGIN { IGNORECASE = 1; print ("ABC" ~ /abc/), index("ABC", "b") }
