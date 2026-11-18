# stdin: data/match-numbers.txt
# match(s, re, arr): arr[n] is group n, with arr[n, "start"] and arr[n, "length"]; the captured
# text of a numeric field is a strnum (gawk match1, match3).
BEGIN {
    data = "foooobazbarrrrr"
    match(data, /(fo+).+(bar*)/, arr)
    for (i = 0; i in arr; i++) {
        printf "arr[%d] = \"%s\"\n", i, arr[i]
        printf "arr[%d, \"start\"] = %s, arr[%d, \"length\"] = %s\n", i, arr[i, "start"], i, arr[i, "length"]
    }
    print match("abc", /(b)c/, m), m[0], m[1], m[1, "start"]
}
{ match($0, /.*/, a); print a[0], (a[0] == a[0] + 0 ? "OK" : "FAULT") }
