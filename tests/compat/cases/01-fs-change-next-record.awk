# stdin: data/passwd.txt
# FS assigned in an action takes effect from the next record
{ print $1; FS = ":" }
