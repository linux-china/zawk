# match() used as a statement still sets RSTART and RLENGTH.
# stdin: data/match-statement.txt
BEGIN {
    match("abc", /b/); print RSTART, RLENGTH
    match("abc", /z/); print RSTART, RLENGTH
    s = "key=value"; re = "=[a-z]+"
    match(s, re); print substr(s, RSTART + 1, RLENGTH - 1)
}
{ match($0, /[0-9]+/); print RSTART, RLENGTH, substr($0, RSTART, RLENGTH) }
