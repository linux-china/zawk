# stdin: data/printf-c-strnum.txt
# opts: -v n=68
# %c formats a strnum (field, split element, getline var, -v value) that looks like a number
# as the character with that code, and any other string by its first character.
BEGIN {
    printf "%c|", n
    x = "66"; printf "%c|", x
    split("67 abc 0x41", a); printf "%c%c%c\n", a[1], a[2], a[3]
}
{
    printf "[%c][%3c][%c][%c]", $1, $1, $1 "", $2
    s = sprintf("%c", $1); print s
}
END {
    while ((getline line < "data/printf-c-strnum.txt") > 0) printf "%c", line
    print ""
}
