# stdin: data/icase-fs.txt
# IGNORECASE applies to a regex FS, gsub and match (gawk igncfs, ignrcase, icasefs).
BEGIN { IGNORECASE = 1; FS = "x|q" }
{ print NF, $1, $2 }
END {
    s = "Hello HELLO hello"; print gsub(/hello/, "hi", s), s
    print match("xxABC", /abc/), RSTART, RLENGTH
    print split("1A2a3", parts, /a/), parts[3]
    IGNORECASE = 0
    s = "Hello HELLO hello"; print gsub(/hello/, "hi", s), s
}
