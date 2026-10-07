BEGIN { r = system("exit 3"); print r; system("echo from-system") }
