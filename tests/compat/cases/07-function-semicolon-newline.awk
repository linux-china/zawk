# A function definition may be followed by a semicolon, and a newline may follow the closing
# parenthesis of its parameter list (gawk funsemnl).
function hello(x)
{ return "hi " x };
BEGIN { print hello("there") } ;
function twice(y) { return y y } BEGIN { print twice("ab") }
