# Reading from two command pipes in turn, closing one after each line; an empty command output
# and /dev/null as input are not errors (gawk pipeio1, iobug1, getline3).
BEGIN {
    cmd1 = "printf '1\\n2\\n'"; cmd2 = "echo two"
    while ((cmd1 | getline) > 0) {
        cmd2 | getline L
        close(cmd2)
        print $0, L
    }
    close(cmd1)
    printf "" | "cat"
    close("cat")
    print (getline line < "/dev/null"), "[" line "]"
    print ("true" | getline line), "[" line "]"
}
