# `^` in a regex FS only matches at the start of the record (gawk fscaret).
BEGIN {
    FS = "^."; OFS = "|"
    $0 = "xabc"; print NF, "[" $1 "]", "[" $2 "]"
    $0 = "a.b.c"; $1 = $1; print
}
