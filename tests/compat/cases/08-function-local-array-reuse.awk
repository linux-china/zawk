# A local parameter used as an array is fresh on every call (gawk prmreuse, paramtyp).
function fill(n,    scratch, i, s) {
    for (i = 1; i <= n; i++) scratch[i] = i * i
    s = length(scratch)
    split("1 2 3", scratch)
    return s ":" length(scratch)
}
function setone(d) { d[1] = 1 }
function setzero(d) { d[1] = 0 }
BEGIN {
    print fill(5), fill(1), fill(3)
    setone(arr); print arr[1]; setzero(arr); print arr[1]
}
