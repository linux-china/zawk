# stdin: data/emp.data
# Anchored patterns must match in full, not just their first character.
/^Ka/ { print "Ka:", $1 }
/^K[a-z]t/ { print "K?t:", $1 }
$1 ~ /^Ma$/ { print "never" }
BEGIN { print ("ac" ~ /^ab/), ("ab" ~ /^a$/), ("a.c" ~ /^a\./), ("abc" ~ /^a\./) }
