BEGIN { print srand(5), srand(7); print (srand() == 7); t = srand(); print (t >= 1000000000), (rand() < 1) }
