# stdin: data/emp.data
# `length` without parentheses is length($0) in any expression position; names that merely
# contain "length" are ordinary variables.
{
    x = length
    lengthy = length + 1
    print length, x, lengthy, length "|" length $1, (length < 25), !length
}
