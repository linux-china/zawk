# stdin: data/countries
$4 !~ /America/ { n++ } END { print n }
