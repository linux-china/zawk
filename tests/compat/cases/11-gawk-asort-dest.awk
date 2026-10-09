# asort() with a destination array, string-keyed and mixed-type sources, as a statement, and on
# empty arrays.
BEGIN {
    n = split("10 9 abc 2 b10 -1 Abc", words)
    m = asort(words, sorted)
    for (i = 1; i <= m; i++) printf "%s ", sorted[i]
    print m, words[1], length(words)

    h["x"] = 3; h["y"] = 1; h["z"] = "b"; h["w"] = 2.5
    asort(h)
    print h[1], h[2], h[3], h[4], ("x" in h), length(h)

    old["stale"] = 1; f[1] = 2.5; f[2] = -1.5
    print asort(f, old), length(old), ("stale" in old), old[1], old[2]

    print asort(empty), length(empty), asort(empty, empty2), length(empty2)
}
