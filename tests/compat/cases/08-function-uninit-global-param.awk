# An uninitialized global passed as a parameter is copied; assigning to the global inside the
# function does not change the parameter (gawk paramuninitglobal).
function g(x) {
    a = 10; x = 90
    print x, a
    a++; x++
    print x, a
}
BEGIN { g(a); print a }
