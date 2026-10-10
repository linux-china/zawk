# stdin: data/strnum-truth.txt
# opts: -v zero=0 -v one=1
# A string that may be a strnum (input, -v, split, getline, copies of them, numbers converted to
# strings) is false when it looks like a number equal to zero; any other string is false only
# when it is empty.
BEGIN {
    print (zero ? "t" : "f"), (one ? "t" : "f"), !zero, !one
    split("0 1", p); print (p[1] ? "t" : "f"), (p[2] ? "t" : "f")
    "echo 0.0" | getline g; close("echo 0.0"); print (g ? "t" : "f")
    # Constants and results of string operations are never strnums.
    s = "0"; print (s ? "t" : "f"), !s, ("0" ? "t" : "f")
    # A number assigned to a variable inferred to be a string is still a number.
    x = "a"; x = 0; print (x ? "t" : "f"), !x
    a[1] = "s"; a[2] = 0; print (a[1] ? "t" : "f"), (a[2] ? "t" : "f")
    print (f(0) ? "t" : "f"), (f(1) ? "t" : "f")
    done = "no"; done = 0; while (!done) if (++i > 3) done = 1; print i
}
function f(n) { if (n) return "s"; return 0 }
$1 { t1++ }
!$1 { f1++ }
{
    printf "[%s] %s %s %s %s %s", $1, ($1 ? "t" : "f"), ($0 ? "t" : "f"), !$1, ($1 && 1), ($1 || 0)
    v = $1; n = 0; while (v) { v = 0; n++ }
    printf " %d %s %s\n", n, (($1 "") ? "t" : "f"), (substr($1, 1) ? "t" : "f")
}
END { print t1, f1 }
