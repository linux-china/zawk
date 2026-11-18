# stdin: data/fldterm.txt
# A digit used as FS splits inside a number, and the field converts to the shorter number
# (gawk fldterm).
BEGIN { FS = "3" }
{ print $1 + 0; print $1; print NF, "[" $2 "]", $2 + 0 }
