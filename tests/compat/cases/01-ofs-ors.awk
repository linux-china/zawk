# stdin: data/emp.data
BEGIN { OFS = "|"; ORS = ";\n" } { print $1, $2 }
