BEGIN { print ("a]" ~ /[]a]+/), ("-" ~ /[a-]/), ("5" ~ /[[:digit:]]/), ("x" ~ /[^[:alpha:]]/), ("TAB\t" ~ /[[:space:]]/) }
