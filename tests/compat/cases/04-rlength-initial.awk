BEGIN { print RSTART, RLENGTH; match("abc", /z/); print RSTART, RLENGTH }
