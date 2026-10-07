BEGIN { s = "hello"; print substr(s, 2, 3), substr(s, 2), substr(s, 0), substr(s, -1, 3), substr(s, 4, 100), "[" substr(s, 10) "]", substr(s, 1.5, 2.3) }
