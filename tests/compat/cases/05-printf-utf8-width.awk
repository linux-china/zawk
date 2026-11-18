# In a UTF-8 locale, %s precision and width count characters, and %c of a code point prints the
# character.
BEGIN {
    s = "héllo"
    printf "[%.2s][%7s][%-7s]\n", s, s, s
    printf "[%c][%c][%3c]\n", 233, "日本", "é"
    print length(sprintf("%5s", "日本"))
}
