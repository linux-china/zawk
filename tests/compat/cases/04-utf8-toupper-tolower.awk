# toupper/tolower convert non-ASCII letters too; characters whose mapping is not a single
# character (like "ß") are kept, as with C's towupper/towlower.
BEGIN {
    print toupper("ÄbcÉ àéîõü"), tolower("ÄBC ÀÉÎÕÜ")
    print toupper("αβγ привет"), tolower("ΑΒΓ ПРИВЕТ")
    print toupper("straße"), toupper("日本語abc"), tolower("中文ABC")
    s = toupper("äöü"); print length(s), s
}
