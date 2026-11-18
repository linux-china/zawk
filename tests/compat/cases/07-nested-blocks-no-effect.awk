# stdin: data/three-lines.txt
# Nested blocks, empty statements and expression statements without effect are valid
# (gawk nested, noeffect).
BEGIN { total = 0 }
{
    total++
    {
        split($0, parts, " ")
        last = parts[2]
        { ; }
    }
    ;;
    s == "x"
    s + 1
    print parts[1], last
}
END { { { print total } } }
