# stdin: data/numbers.txt
{ print ($1 < 5) ? "lt" : "ge" }
