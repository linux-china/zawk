# exit: 1
# Comparison operators are non-associative: a chain without parentheses is a syntax error.
BEGIN { print (3 < 2 < 1) }
