# "%%" prints a single "%" and does not consume an argument, in printf and sprintf.
BEGIN {
    printf "%d%%\n", 5
    printf "%%%d%% %s\n", 7, "x"
    printf("%s|%5.1f%%\n", "p", 12.345)
    s = sprintf("%d%% done", 42)
    print s
    print sprintf("100%%")
}
