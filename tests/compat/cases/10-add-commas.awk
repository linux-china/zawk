# stdin: data/amounts.txt
# Format numbers with two decimals and thousands separators.
function commas(x,    s) {
    if (x < 0) return "-" commas(-x)
    s = sprintf("%.2f", x)
    while (s ~ /[0-9][0-9][0-9][0-9]/) sub(/[0-9][0-9][0-9][,.]/, ",&", s)
    return s
}
{ printf "%-12s %16s\n", $0, commas($0) }
