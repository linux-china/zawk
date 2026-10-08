# stdin: data/numbers.txt
# getline var in BEGIN reads whole records from the main input, even when no field is used.
BEGIN { getline first; getline second; print "[" first "][" second "]", NR }
