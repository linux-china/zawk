# In BEGIN, $0 and the fields are empty; setting NF creates empty fields (gawk uninit4).
function show() { print "[" $0 "]" }
BEGIN {
    show()
    print "[" $0 "]", "[" $(1 - 1) "]", "[" $1 "]", NF
    NF = 3
    print "[" $2 "]", "[" $0 "]", NF
}
