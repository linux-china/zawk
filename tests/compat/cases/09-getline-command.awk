BEGIN { "echo hello world" | getline; print $2; "echo a b" | getline v; print v, NF }
