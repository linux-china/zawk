BEGIN { print ("aaa" ~ /^a{3}$/), ("aa" ~ /^a{3}$/), ("abab" ~ /^(ab){2}$/) }
