# stdin: data/colon-space.txt
# args: FS=[[:blank:]:]+ -
# A `var=value` operand sets FS before the following input is read (gawk fsspcoln).
{ print NF, $2, $NF }
