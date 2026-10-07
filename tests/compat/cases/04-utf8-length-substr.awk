# stdin: data/utf8.txt
{ print length($0), substr($0, 2, 3), index($0, "l") }
