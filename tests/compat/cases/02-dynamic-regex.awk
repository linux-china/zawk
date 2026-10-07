# stdin: data/countries
BEGIN { re = "^C" } $1 ~ re { print $1 }
