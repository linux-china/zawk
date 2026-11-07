# IGNORECASE is on when its value is true as in `if (x)`: non-empty non-numeric strings and
# non-integral numbers turn it on.
# stdin: data/ignorecase.txt
BEGIN {
    IGNORECASE = "yes"; print ("A" ~ /a/)
    IGNORECASE = ""; print ("A" ~ /a/)
    IGNORECASE = 0.5; print ("A" ~ /a/)
    IGNORECASE = 0; print ("A" ~ /a/)
    x = "on"; IGNORECASE = x; print ("A" ~ /a/)
    IGNORECASE = 0
}
NR == 1 { IGNORECASE = $1; print ("A" ~ /a/) }
