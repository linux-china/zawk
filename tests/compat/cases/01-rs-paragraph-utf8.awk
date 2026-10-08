# stdin: data/para-utf8.txt
# Paragraph mode trims the newline at the end of the last record, also when it is not ASCII.
BEGIN { RS = "" }
{ print NR ": <" $0 ">", NF }
