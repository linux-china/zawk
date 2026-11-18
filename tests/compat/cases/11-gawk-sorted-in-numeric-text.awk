# stdin: data/match-numbers.txt
# With @ind_num_asc, keys sort by numeric value but keep their text; copying an array while
# walking it in order, and asort() of an empty array (gawk sortfor2, sortempty).
BEGIN { PROCINFO["sorted_in"] = "@ind_num_asc" }
{ A[$1] = NR }
END {
    for (I in A) B[I] = A[I]
    for (I in B) printf "%s=%s ", I, B[I]
    print ""
    print asort(empty), length(empty), asorti(empty2)
}
