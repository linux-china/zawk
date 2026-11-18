# strftime() and mktime() in UTC with the utc-flag argument (gawk strftime, mktime).
BEGIN {
    print strftime("%Y-%m-%d %H:%M:%S", 0, 1)
    print strftime("%Y-%m-%d %H:%M:%S %j %a %b", 86400 * 366 + 3661, 1)
    t = mktime("2020 02 29 12 30 45", 1); print t, strftime("%Y/%m/%d %T", t, 1)
    print mktime("1970 01 01 00 00 00", 1), mktime("2021 13 01 00 00 00", 1) == mktime("2022 01 01 00 00 00", 1)
    print mktime("garbage", 1)
}
