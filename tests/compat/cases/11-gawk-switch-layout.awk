# Layouts of a switch statement: newlines, semicolons, one-liners and comments.
BEGIN {
    switch (1) { case 1: print "one-liner"; break; default: print "no" }

    switch (2)
    {
        case 1:
            print "no"
            break

        # A comment between cases.
        case 2:   # and after a label
            print "two"
            break
    }

    switch ("a") {
    case "a": print "a"
    case "b": print "b" }

    switch (3) {
    case 3:
    ;
    default:
    }
    print "empty bodies"

    switch (4) { case 4: if (1) print "if"; else print "else"; break }

    switch (5) {
    case 5:
        for (i = 0; i < 2; i++) print "for", i
    case 6:
        while (j < 2) print "while", j++
    }
}
