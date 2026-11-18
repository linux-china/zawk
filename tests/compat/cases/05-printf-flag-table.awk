# Every combination of the 0, #, space, + and - flags with integer, octal, hex and float
# conversions (gawk hsprint, printfloat).
BEGIN {
    nflag = split("|0|#|#0| | 0| #| #0|+|+0|+#|+#0|-|-#|- |-+", flag, "|")
    nconv = split("5d 5o 5x 7.2f 10.2e 10.4g 10.4G 5i", conv, " ")
    value[1] = 45; value[2] = 45; value[3] = 45; value[4] = 12.678; value[5] = 12.678
    value[6] = 12.678; value[7] = -3.4567; value[8] = -7
    for (i = 1; i <= nflag; i++) {
        line = sprintf("%5s|", "%" flag[i])
        for (j = 1; j <= nconv; j++)
            line = line sprintf("%" flag[i] conv[j] "|", value[j])
        print line
    }
}
