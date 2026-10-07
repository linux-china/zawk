BEGIN { print gensub(/(a)(b)/, "\\2\\1", "g", "abab"), gensub(/o/, "0", 2, "foo boo") }
