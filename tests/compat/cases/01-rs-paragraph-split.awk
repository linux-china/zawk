# In paragraph mode, newline separates fields with a single-character FS, and split() with the
# default separator does the same (gawk rstest1, rstest2, fsrs).
BEGIN {
    RS = ""; FS = ":"
    s = "a:b\nc:d"
    n = split(s, parts)
    print n, length(parts[2]), parts[2], parts[3]
    FS = "\\"
    $0 = "a\\b"
    print NF, $1, $2
}
