# How the switch value is compared with labels: numbers numerically, strings as strings, input
# that looks numeric (strnum) numerically, and uninitialized values as both 0 and "".
# stdin: data/numbers.txt
{
    v = $1
    switch ($1) {
    case 10:      r = "ten"; break
    case "3.5":   r = "string 3.5"; break
    case -2:      r = "minus two"; break
    case 1000:    r = "thousand"; break
    case 42:      r = "forty-two"; break
    case /abc/:   r = "has abc"; break
    case 7:       r = "seven"; break
    case "":      r = "empty"; break
    default:      r = "default"
    }
    print NR ": [" $0 "] -> " r
}

END {
    # A string constant compares as a string with a string label.
    s = "1.0"
    switch (s) {
    case 1: print "s: number"; break
    case "1.0": print "s: string"; break
    }

    # A number compares numerically with a numeric label, and as its string with a string label.
    n = 1.0
    switch (n) {
    case "1.0": print "n: string 1.0"; break
    case 1: print "n: number"; break
    }
    switch (n) {
    case "1": print "n: string 1"; break
    }

    # An uninitialized variable equals both 0 and "".
    switch (unset1) {
    case 0: print "unset1: zero"; break
    case "": print "unset1: empty"; break
    }
    switch (unset2) {
    case "": print "unset2: empty"; break
    case 0: print "unset2: zero"; break
    }
    switch (missing["k"]) {
    case "": print "missing element: empty"; break
    }

    # Numeric variables, with numeric and string labels.
    count = 5
    switch (count) {
    case "5": print "count: string 5"; break
    }
    switch (count * 2) {
    case 10: print "count*2: 10"; break
    }

    # The result of a comparison.
    switch (3 > 2) {
    case 1: print "true is 1"
    }
}
