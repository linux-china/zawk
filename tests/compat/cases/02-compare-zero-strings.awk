# Strnums such as "00" from split() or fields compare equal to the number 0 (gawk compare2,
# strtod).
BEGIN {
    split("00/00/00", mdy, "/")
    print (mdy[1] == 0) && (mdy[2] == 0) && (mdy[3] == 0), (mdy[1] == "0"), (mdy[1] < 1)
}
