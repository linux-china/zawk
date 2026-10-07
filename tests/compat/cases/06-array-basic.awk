# stdin: data/emp.data
{ pay[$1] = $2 * $3 } END { print pay["Kathy"], pay["Mary"], length(pay) }
