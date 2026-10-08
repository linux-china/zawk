# exit: 1
# The target of an output redirection cannot be a conditional expression: this is a syntax
# error, not a write to the file "a".
BEGIN { print 1 > 2 ? "a" : "b" }
