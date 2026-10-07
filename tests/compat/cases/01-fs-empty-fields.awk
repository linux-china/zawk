# stdin: data/passwd.txt
BEGIN { FS = ":" } { print NF ": [" $5 "]" }
