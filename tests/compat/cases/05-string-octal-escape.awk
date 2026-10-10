# octal escapes take at most three octal digits; 8, 9 and letters end the escape
BEGIN {
  s = "\0b"; print length(s), (substr(s, 2) == "b")
  s = "\1a"; print length(s), (substr(s, 2) == "a")
  s = "\08"; print length(s), (substr(s, 2) == "8")
  print "x\101\1029y"
  print "[\61z]"
}
