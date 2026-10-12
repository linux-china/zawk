# stdin: data/ignorecase.txt
BEGIN { IGNORECASE = 1 }
$1 == "apple" { eq++ }
$1 < "b" { lt++ }
index($0, "PIE") { ix = ix " " NR }
{
    switch ($1) {
    case "banana": b++; break
    case /^ch/: c++; break
    }
}
END {
    print eq, lt, ix, b, c
    print split("aXbxc", a, /x/), split("aXbxc", a, "x")
    IGNORECASE = 0
    print ("ABC" == "abc"), index("ABC", "b"), split("aXbxc", a, /x/)
}
