# opts: -v n=10
# args: 10 9
# -v values and ARGV elements are strnums.
BEGIN { print (n > 9), (n "" > "9"), (ARGV[1] > ARGV[2]) }
