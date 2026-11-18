# The field number may be a string or a strnum: $"2", $"1" "0" (gawk strfieldnum).
BEGIN {
    $0 = "a b c d e f g h i j k"
    print $"2", $("1" "0"), $" 3 ", $(1 + "1")
    n = split("4", s); print $s[1]
}
