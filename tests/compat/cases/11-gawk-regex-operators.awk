# gawk regex operators \s \S \w \W \B \y and the buffer anchors (gawk gsubtst6, gnureops).
BEGIN {
    x = "abcd"; gsub(/\B/, ":", x); print x
    s = "a b\tc"; print gsub(/\s/, "_", s), s
    t = "foo-bar baz"; print gsub(/\w+/, "W", t), t
    u = "a1!b2?"; print gsub(/\W/, "", u), u
    v = "cat concat cats"; print gsub(/\ycat\y/, "DOG", v), v
    print ("ab" ~ /\`ab\'/), ("x\nab" ~ /\`ab/), ("S" ~ /\S/), (" " ~ /\S/)
}
