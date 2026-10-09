# User-defined functions named like zawk's stdlib extensions (trim, max, min, abs, round, join,
# repeat) take precedence over them, so awk programs that define these helpers keep working.
function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
function max(a, b) { return a > b ? a : b }
function min(a, b) { return a < b ? a : b }
function abs(v) { return v < 0 ? -v : v }
function round(x) { return int(x + 0.5) }
function repeat(s, n,  r) { while (n-- > 0) r = r s; return r }
function join(a, n, sep,  s, i) { for (i = 1; i <= n; i++) s = s (i > 1 ? sep : "") a[i]; return s }
function label(s) { return "<" trim(s) ">" }
BEGIN {
    print "[" trim("  a b  ") "]", label(" x ")
    print max(3, 10), min(3, 10), max("b", "a"), abs(-3), abs(2.5), round(2.6)
    print repeat("ab", 3)
    n = split("a b c", parts); print join(parts, n, "-")
}
