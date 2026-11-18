# With a single-character RS other than newline, newlines inside a record separate fields under
# the default FS (gawk nlfldsep).
BEGIN {
    RS = "A"
    while (("printf 'x y\\nzAp\\nqA'" | getline) > 0) {
        print NF
        for (i = 1; i <= NF; i++) print "[" $i "]"
    }
}
