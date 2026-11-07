# stdin: data/a.txt
function m(s) { sub(/a/, "b", s) }
function n(s) { if (s ~ /a/) sub(/a/, "b", s); print s }
function g(s) { gsub(/a/, "c", s); return s }
function loc(  u) { u = "aa"; m(u); n(u); print u, g(u), u }
BEGIN { t = "aa"; m(t); print t; n(t); n(t); print t; print g(t), t; loc() }
function h(s) { gsub(/n/, "N", s); print s }
{ h($0); print $0; x = $0; h(x); print x }
