# `func` is a synonym for `function` in gawk.
func sq(x) { return x * x }
func greet(name) { return "hi " name }
BEGIN { print sq(7), greet("you") }
