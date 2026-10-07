function fact(n) { return n <= 1 ? 1 : n * fact(n - 1) } function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2) } BEGIN { print fact(10), fib(15) }
