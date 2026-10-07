# stdin: data/para.txt
BEGIN { RS = "" } { print NR ": " $1 " " $2 " (" NF " fields)" }
