# stdin: data/fs-change.txt
# A new FS applies from the next record, even when the fields of the current record have not
# been accessed yet (zawk splits records lazily).
NR == 1 { FS = ","; print $1 "|" NF; $3 = "X"; print; next }
NR == 2 { print $1 "|" NF; FS = " "; print $2; $0 = $0; print $2; next }
{ print $2 }
