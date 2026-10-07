BEGIN { while ((getline line < "data/b.txt") > 0) n++; print n, line; close("data/b.txt"); getline line < "data/b.txt"; print line }
