# stdin: data/lines.txt
# Swap pairs of lines with `getline var`, which does not touch $0 (gawk swaplns).
{
    if ((getline tmp) > 0) { print tmp; print } else print
}
END { print NR }
