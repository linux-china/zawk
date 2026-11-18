# args: data/join-left.txt data/join-right.txt
# Join two files on their first field with the NR == FNR idiom.
NR == FNR { name[$1] = $2; next }
$1 in name { print $1, name[$1], $2; next }
{ print $1, "(none)", $2 }
