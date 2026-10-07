# stdin: data/emp.data
{ if ($3 == 0) print $1, "none"; else if ($3 < 20) print $1, "some"; else print $1, "many" }
