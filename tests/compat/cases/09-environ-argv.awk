# args: data/a.txt
BEGIN { print ARGC, ARGV[1], (length(ENVIRON["PATH"]) > 0) }
