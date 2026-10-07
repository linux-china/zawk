# stdin: data/para.txt
BEGIN { RS = ""; FS = "\n" } { print NR, NF, $1 }
