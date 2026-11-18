# stdin: data/zero-forms.txt
# Fields that look like a zero in any notation (00, 0e0, .0e+0, -0, ...) are false; other
# non-empty fields are true (gawk strtod).
{ for (i = 1; i <= NF; i++) print $i, ($i ? "true" : "false"), $i + 0 }
