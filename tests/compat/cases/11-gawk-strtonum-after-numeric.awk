# strtonum() reads octal and hex prefixes even when the string was already used as a number
# (gawk strtonum, strtonum1).
BEGIN {
    print strtonum("0x13"), strtonum("013"), strtonum("13"), strtonum(13), strtonum("0X1f")
    x = "011"
    print x + 0
    print strtonum(x), x + 0
    print strtonum("-0x10"), strtonum(" 017"), strtonum("08"), strtonum("1e2")
}
