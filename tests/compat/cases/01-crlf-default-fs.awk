# stdin: data/crlf.txt
# With the default FS, gawk splits on blanks and newlines only, so `\r` stays in the last field
# and a line holding only `\r` has one field. zawk also treats `\r` as a blank (see known_failures.txt).
function show(s) { gsub(/\r/, "\\r", s); return s }
{
    printf "%d:", NF
    for (i = 1; i <= NF; i++) printf "[%s]", show($i)
    $1 = $1
    print " rebuilt=[" show($0) "]"
}
END {
    n = split("x\ry z\r", a)
    printf "split=%d:", n
    for (i = 1; i <= n; i++) printf "[%s]", show(a[i])
    print ""
}
