# printf follows C: flags, `*` widths and precisions, all conversions, and gawk's handling of
# out-of-range integers, infinities, %c and UTF-8 strings.
BEGIN {
    printf "%e %E %.2e %g %G %g %g %#g\n", 12345.678, 1e-10, 0, 100000, 1e-10, 0.0001, 123456789, 1
    printf "%i %u %X %#x %#o %+d % d %+.2f\n", 3.9, -1, 255, 255, 8, 5, 5, 2.5
    printf "[%*d][%-*d][%.*f][%*.*f][%*d]\n", 5, 42, 4, 42, 2, 3.14159, 8, 3, 3.14159, -4, 7
    printf "[%05s][%-5s|][%.2s][%5.1s][%c][%c][%c][%5c]\n", "ab", "ab", "héllo", "xyz", "hello", 65, 256, "x"
    printf "[%d][%d][%d][%5.3d][%x][%o][%5%][%z]\n", 2^63, -3.9, "12abc", 7, -1, -8
    printf "[%f][%5.1f][%d]\n", -log(0), -log(0), -log(0)
    printf "%5s|%-5s|%.1s|\n", "你好", "你好", "你好"
}
