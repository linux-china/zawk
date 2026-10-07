# stdin: data/comments.txt
/\/\*/, /\*\// { print NR ": " $0 }
