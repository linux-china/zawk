BEGIN { s = "héllo"; print match(s, /l+/), RSTART, RLENGTH, substr(s, RSTART, RLENGTH) }
