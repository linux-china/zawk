# stdin: data/b.txt
{ for (i = 1; i <= length($0); i++) c[substr($0, i, 1)]++ } END { print c["i"], c["r"], c["s"], c["z"] + 0 }
