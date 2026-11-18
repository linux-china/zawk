# %g switches to exponent form based on the exponent and precision; %e uses at least two
# exponent digits.
BEGIN {
    n = split("100000 1000000 0.0001 0.00001 123456789 1e-10 0.5 100 1e100 -0.000123456", v, " ")
    for (i = 1; i <= n; i++) printf "%g|%.3g|%#g|%G|%e\n", v[i], v[i], v[i], v[i], v[i]
    printf "%.0g %.0e %#.0e %#.0f %.20f\n", 2.5, 12345, 3, 3, 0.1
    printf "%e %E\n", 1e-300, 1.5e300
}
