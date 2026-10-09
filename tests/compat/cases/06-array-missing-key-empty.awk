# stdin: data/emp.data
# `a[k] == ""` is true for a missing element even when the array holds numbers.
{ count[$1]++; rate[$1] = $2; name[$1] = $1 }
END {
    print (count["Nobody"] == ""), (count["Beth"] == ""), ("" == count["Nemo"]), (count["Dan"] != "")
    print (rate["Nobody"] == ""), (rate["Beth"] == ""), (name["Nobody"] == ""), (name["Beth"] == "")
    z["k"] = 0; i = 1; r = (z[i++] == "")
    print (z["k"] == ""), r, i, length(count), length(z)
}
