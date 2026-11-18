# stdin: data/asgext.txt
# Assigning a field beyond NF on a short record extends it with empty fields (gawk asgext).
{ print $3; $4 = "a"; print; print NF }
