# stdin: data/igncase-dyn.txt
# IGNORECASE can change for each match, also for dynamic regexes and bracket classes
# (gawk igncdym, ignrcas2).
BEGIN {
    pattern[1] = "bar"; ignore[1] = 1
    pattern[2] = "foo"; ignore[2] = 0
}
{
    for (i = 1; i <= 2; i++) {
        IGNORECASE = ignore[i]
        print match($0, pattern[i]) " " pattern[i] ":" $0
    }
}
END {
    IGNORECASE = 1
    print ("a" ~ /[[:alnum:]]/), ("A" ~ /[[:lower:]]/), ("q" ~ /[A-Z]/), ("Q" ~ "^[a-z]$")
    IGNORECASE = 0
    print ("A" ~ /[[:lower:]]/), ("q" ~ /[A-Z]/)
}
