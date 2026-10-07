# stdin: data/countries
{ printf "%-10s %6d %5d %s\n", $1, $2, $3, $4 } END { printf "%s\n", "----" }
