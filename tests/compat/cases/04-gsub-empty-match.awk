BEGIN { s = "abc"; gsub(/x*/, "-", s); print s }
