# stdin: data/strnum.txt
# String constants, concatenations and string function results always compare as strings;
# uninitialized values compare as "" or 0.
function gt(a, b) { return a > b }
{
    print ("10" > "9"), ("10" > 9), ($1 > "9"), ($1 "" > $2 ""), (substr($1, 1) > substr($2, 1))
    print gt($1, $2), (u < $1), (u == $5), (u == 0), (u == "")
}
