BEGIN { s = "a.b|c"; print (s ~ /a\.b\|c/), ("axb" ~ /a.b/), ("ab" ~ /^(ab|cd)+$/), ("abab" ~ /^(ab)*$/), ("" ~ /^$/) }
