# stdin: data/lines.txt
# A bare regex is a match against $0 that yields 0 or 1 and can be used in arithmetic.
{ total += /[135]/ * 10 + /line/ }
/2/, /3/ { range = range NR }
END { print total, range }
