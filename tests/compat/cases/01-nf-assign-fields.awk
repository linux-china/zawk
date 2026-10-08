# stdin: data/emp.data
# Assigning NF rebuilds $0 even when only some fields are referenced; NF=0 empties the record,
# and NF=NF rebuilds it with OFS.
NR == 1 { NF = 2; print $1 "|" $2 "|" $3 "|" NF }
NR == 2 { NF = 0; print "[" $0 "]", NF }
NR == 3 { OFS = "-"; NF = NF; print; NF++; $NF = "x"; print }
NR == 4 { NF--; print; print NF }
