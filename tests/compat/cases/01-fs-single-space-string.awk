BEGIN { FS = " "; $0 = "  a   b  "; print NF, $1 }
