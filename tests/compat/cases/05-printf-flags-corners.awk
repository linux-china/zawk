# printf flags, precision on integers and zero values (gawk printf1, intprec, pcntplus, zeroflag).
BEGIN {
    fmt[1] = "%8.5d"; data[1] = 100
    fmt[2] = "%#o"; data[2] = 0
    fmt[3] = "%#.1o"; data[3] = 0
    fmt[4] = "%#x"; data[4] = 0
    fmt[5] = "%.0d"; data[5] = 0
    fmt[6] = "%5.0d"; data[6] = 0
    fmt[7] = "%#o"; data[7] = 8
    fmt[8] = "%#x"; data[8] = 255
    fmt[9] = "%#X"; data[9] = 255
    fmt[10] = "%-05d"; data[10] = 7
    fmt[11] = "% d"; data[11] = 7
    fmt[12] = "%+5d"; data[12] = -7
    fmt[13] = "%05.1f"; data[13] = -2.25
    fmt[14] = "%i"; data[14] = 3.9
    fmt[15] = "%u"; data[15] = 3.9
    for (i = 1; i <= 15; i++) printf("%s|" fmt[i] "|\n", fmt[i], data[i])
    printf "%.10d:%.10x\n", 5, 14
    printf "%+d %d\n", 3, 4
}
