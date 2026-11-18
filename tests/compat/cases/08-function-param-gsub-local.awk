# stdin: data/three-lines.txt
# gsub() on a scalar parameter changes the local copy only; a global modified by the caller
# keeps its value (gawk manglprm, inpref).
function protect(s) { gsub("\n", "\\n", s); gsub(/ /, "_", s); return s }
function process(s,   n, parts) {
    print "Buffer = [" protect(Buffer) "]"
    Buffer = Buffer s "\n"
    n = split(Buffer, parts, "\n")
    return n
}
{ print process($0) }
END { print length(Buffer) }
