# `getline < "a" "b"` reads from "a" and concatenates "b"; `cmd | getline x y` concatenates y to
# the result (gawk inputred, getline3).
BEGIN {
    print getline < "no-such-file" ".txt"
    cmd = "echo 3"; y = 7
    print (cmd | getline x y), x
    close(cmd)
    print (cmd | getline x) y
}
