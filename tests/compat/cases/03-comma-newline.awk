# A newline may follow the comma in parameter lists, subscripts, `(i, j) in a` and
# parenthesized print argument lists.
function add(a,
             b,
             c) {
    return a + b + c
}
BEGIN {
    print add(1,
              2, 3)
    a[1,
      2] = 3
    if ((1,
         2) in a)
        print "in", a[1, 2]
    print (1, # comment with ) and "
           2) | "cat"
    close("cat")
    printf ("%s-%s\n",
            "x", "y") | "cat"
    close("cat")
}
