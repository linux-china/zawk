BEGIN { s = "cat hat"; gsub(/[ch]at/, "[&]", s); print s; t = "a.b"; gsub(/\./, "\\&", t); print t }
