# args: data/a.txt data/b.txt
FNR == 1 { files++ } END { print files, NR }
