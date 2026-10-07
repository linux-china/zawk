BEGIN { FS = "[,;]+" ; $0 = "a,b;;c,,;d"; print NF, $3, $4 }
