BEGIN { srand(42); a = rand(); srand(42); b = rand(); print (a == b), (a >= 0 && a < 1) }
