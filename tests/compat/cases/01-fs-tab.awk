# stdin: data/countries
BEGIN { FS = "\t" } { print $4 }
