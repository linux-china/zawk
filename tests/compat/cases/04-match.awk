BEGIN { print match("foobar", /o+/), RSTART, RLENGTH; print match("foobar", /z/), RSTART, RLENGTH }
