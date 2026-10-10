# int() converts its argument with the usual numeric rules (exponents included), then truncates
BEGIN {
  print int("1e3"), int("2.5e1"), int(".9e1"), int("-3.7"), int("abc"), int(x)
  print int(1e19), int(-3.7), int(7)
  print int("1e400"), int("-1e400")
  s = "12abc"; print int(s), int(s) + 1
}
