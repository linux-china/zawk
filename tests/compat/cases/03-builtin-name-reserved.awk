# exit: 1
# A builtin function name cannot be used as a variable, as in gawk.
BEGIN { toupper = 1; print toupper }
