# stdin: data/rs-caret.txt
# `^` in a regex RS only matches at the start of the input, not after each separator
# (gawk rsstart1, rsstart2).
BEGIN { RS = "^A" }
{ printf "%d [%s]\n", NR, $0 }
END { print NR }
