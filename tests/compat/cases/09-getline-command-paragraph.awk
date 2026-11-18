# `cmd | getline` uses the RS in effect, also RS = "" (paragraph mode); each command is a separate
# stream that continues where it stopped until closed (gawk rstest4, rstest5).
BEGIN {
    RS = ""
    c1 = "printf 'foo\\n\\nbaz\\n'"
    c1 | getline; print
    c1 | getline; print
    print (c1 | getline), $0
    "printf 'bar\\nx\\n\\nbaz\\n'" | getline y; printf "y = <%s>\n", y
    print "x = <" x ">"
}
