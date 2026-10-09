# strtonum(): strings that look like decimal numbers and come from input are decimal; other
# strings with a leading 0 or 0x are octal or hexadecimal.
# stdin: data/strtonum.txt
{ print strtonum($1), strtonum($2), strtonum($3), strtonum($1 ""), strtonum(substr($1, 1)) }
END {
    split("017 0x10", a); print strtonum(a[1]), strtonum(a[2])
    print strtonum("018"), strtonum("017.5"), strtonum("017abc"), strtonum("0X1f"), strtonum(" 017")
}
