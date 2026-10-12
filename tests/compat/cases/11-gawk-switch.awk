# gawk's switch statement: numeric, string and regex labels, default, fall through.
function kind(x) {
    switch (x) {
    case 1:
        return "one"
    case "two":
        return "two"
    case /^th/:
        return "th-word"
    case -1:
        return "minus one"
    case 0x10:
        return "sixteen"
    case 2.5:
        return "two and a half"
    default:
        return "other"
    }
}

BEGIN {
    n = split("1 two three thousand -1 16 2.5 4 x", items, " ")
    for (i = 1; i <= n; i++)
        print items[i], kind(items[i])
    print kind(1), kind(-1), kind(16), kind(2.5), kind(3)

    # Without break, a body falls through to the following ones.
    switch (2) {
    case 1: print "case 1"
    case 2: print "case 2"
    case 3: print "case 3"
    case 4: print "case 4"; break
    case 5: print "case 5"
    }

    # default may come first and falls through too.
    switch ("none") {
    default: print "default"
    case "a": print "a"; break
    case "b": print "b"
    }

    # No matching case and no default: nothing runs.
    switch (42) {
    case 1: print "never"
    }
    print "after no match"

    # An empty switch body.
    switch (1) {
    }
    print "after empty"

    # Labels are tested in order: the first matching one wins.
    switch ("apple") {
    case /p/: print "first regex"; break
    case "apple": print "string"; break
    }

    # The value is evaluated once.
    i = 0
    switch (i++) {
    case 0: print "zero, i =", i; break
    case 1: print "one, i =", i
    }

    # An expression value, an empty body, and a block body.
    switch (substr("hello", 1, 1) "x") {
    case "hx":
    case "hy":
        { print "h-something" }
        break
    }
}
