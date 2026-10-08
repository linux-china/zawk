# A separator must match at least one character: empty matches do not split. A regex literal of
# a single space is a single space, not the default "runs of blanks" separator.
BEGIN {
    print split(" a b ", a, / /), "[" a[1] "]", split(" a b ", a, " "), split("a  b", a, / /)
    print split("a|b", a, /|/), split("abc", a, /x*/), split("axxbc", a, /x*/), a[1], a[2]
    print split("abc", a, /^/), split("abc", a, /$/), split("a1b22c", a, /[0-9]*/), a[3]
    FS = "x*"; $0 = "axxbxc"; print NF, $2
}
