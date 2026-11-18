# Exponentiation: negative exponents, right associativity, precedence over unary minus
# (gawk negexp, negexp2).
BEGIN {
    a = -2
    print 10^a, 2^-1, -2^2, (-2)^2, 2^3^2, 2^0.5 * 2^0.5, 0^0
    x = 2; x ^= 3; print x
    print -2^-2, 4^0.5
}
