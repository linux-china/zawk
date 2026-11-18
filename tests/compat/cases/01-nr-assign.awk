# stdin: data/lines.txt
# NR and FNR may be assigned; counting continues from the new value.
NR == 2 { NR = 10; FNR = 20 }
{ print NR, FNR, $0 }
