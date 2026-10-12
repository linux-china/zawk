# break leaves the switch, continue/next/return/exit act on the enclosing constructs.
# stdin: data/a.txt
function classify(n,    r) {
    r = ""
    switch (n % 3) {
    case 0:
        r = "fizz"
        break
    case 1:
        return "one more"
    default:
        r = "plain"
    }
    return r " " n
}

NR == 1 {
    switch ($0) {
    default:
        print "first record skipped:", $0
        next
    }
}

{ print "record", NR, $0 }

END {
    for (i = 0; i < 6; i++) {
        switch (i) {
        case 1:
            continue
        case 4:
            break
        default:
            printf "%d ", i
        }
        printf "[%d] ", i
    }
    print ""

    for (i = 0; i < 4; i++)
        print classify(i)

    # A break inside a switch does not leave the enclosing loop.
    k = 0
    while (k < 3) {
        k++
        switch (k) {
        case 2: break
        }
        print "loop", k
    }

    # Nested switch statements: break leaves the innermost one.
    switch (1) {
    case 1:
        switch ("x") {
        case "x": print "inner x"; break
        case "y": print "inner y"
        }
        print "outer 1"
        break
    default:
        print "outer default"
    }

    # A switch in for-in and do-while loops.
    a["x"] = 1; a["y"] = 2; a["z"] = 3
    cnt = 0
    for (key in a) {
        switch (key) {
        case "y": continue
        default: cnt += a[key]
        }
    }
    print "sum without y:", cnt

    x = 3
    do {
        switch (x) {
        case 2: print "do two"; break
        default: print "do", x
        }
    } while (--x)
}
