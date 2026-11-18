# Concatenation with uninitialized variables, parenthesized concatenations (gawk concat3, strcat1).
function addb(p) { p = p "b"; return p }
function addc(q) { q = q "c"; return addb(q) }
BEGIN {
    a; a = a (b "c"); print a; print "[" b "]"
    A = "a"; print addc(A), A
    x = 1 2 3; print x, x + 1
    print 1 " " -1, 1 -1, "a" (-1)
}
