function f(a,   tmp) { tmp = a * 2; g = "global"; return tmp } BEGIN { tmp = "outer"; print f(21), tmp, g }
