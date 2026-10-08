# Unary operators bind looser than `^`; concatenation binds looser than arithmetic, and its right
# operand never starts with a unary sign (`a -1` is a subtraction).
BEGIN {
    x = 1; y = 2
    print -2 ^ 2, -x ^ 2, 2 ^ -1, 2 ^ 3 ^ 2, -2 ^ -2, !2 ^ 0
    print 1 " " -1, x " " -1, x " " +y, -1 " " -1
    print 10 " items" + 0, 1 2 * 3, 1 2 ^ 2, 2 3 + 4
    print 1 -1, x -y, x - -y, -"3", - - 3, + + 3, !-1, !!3
    print 1 !0, x !x
    print x++ y, x, y-- x, y
    $0 = "3 4"
    print $1 -$2, $1 " " $2 ^ 2, -$1, $1$2
    print ("a" "b" == "ab"), ("a" "b" < "b"), 1 " " 2 ? "t" : "f"
    print "redirect" | "c" "at"; close("cat")
}
