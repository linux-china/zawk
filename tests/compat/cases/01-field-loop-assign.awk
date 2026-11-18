# stdin: data/matrix.txt
# Assigning each field in a loop rebuilds the record once per assignment (gawk nfloop, tweakfld).
{ for (i = 1; i <= NF; i++) $i = $i * 10; print; print NF }
