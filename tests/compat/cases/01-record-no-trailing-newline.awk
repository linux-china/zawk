# stdin: data/no-newline.txt
# The last record does not need a terminating newline (gawk nonl, datanonl).
{ print NR ": " $NF }
END { print NR, $0 }
