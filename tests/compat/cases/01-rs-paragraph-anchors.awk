# stdin: data/para-blank.txt
# In paragraph mode, `^` and `$` anchor at the start and end of the record, not at embedded
# newlines (gawk anchor).
BEGIN { RS = "" }
{ print NR, (/^A/ || /^C/), /first$/, /^B/, /end$/, /\nB/ }
