# stdin: data/ofs1.txt
# After a field assignment, $0 is rebuilt with the OFS in effect at the time of the assignment;
# changing OFS later does not rebuild it again until another field is assigned (gawk ofs1).
BEGIN { FS = ":" }
FNR == 1 { $2 = "x"; OFS = FS; print; OFS = " " }
FNR == 2 { $2 = "x"; print; OFS = FS; print; OFS = " " }
FNR == 3 { $2 = "x"; print $1; OFS = FS; print; OFS = " " }
FNR == 4 { $2 = "x"; print; $2 = $2; OFS = FS; print }
