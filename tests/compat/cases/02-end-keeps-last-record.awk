# stdin: data/emp.data
# END keeps the last record: $0, NF and the fields (including assignments made to them).
NR == 1 { first = $1 }
{ $3 = "x" $3 }
END { print; print NF, $1, first, NR }
