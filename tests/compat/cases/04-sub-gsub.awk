BEGIN { s = "banana"; n = gsub(/a/, "o", s); print n, s; t = "banana"; sub(/an/, "AN", t); print t }
