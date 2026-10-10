# stdin: data/rs-meta.txt
# opts: -v RS=.
# A single-character RS such as "." separates the main input literally.
{ print NR ": " $0 }
