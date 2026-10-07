BEGIN { RS = ";" } { print NR ": " $0 }
