---
title: zawk Patterns
---

zawk One-Liners - 30 Practical Patterns for Real-World Use.

Quick Summary: Three Core Uses of zawk

1. Field extraction — `zawk '{print $N}'` to slice columns (more powerful than cut)
2. Aggregation — Use END blocks to compute sums, averages, and max/min values
3. Transformation — sub/gsub for regex replacement, printf for formatted output

## Field Manipulation — How Do You Extract and Transform Columns?

### Pattern 1: Print specific fields

```shell
zzawk '{print $2}' file.txt           # 2nd field only
zzawk '{print $1, $3}' file.txt       # 1st and 3rd field, space-separated
zzawk '{print $1 "\t" $3}' file.txt   # Tab-separated output
```

### Pattern 2: Reorder fields

```shell
# Swap 1st and 2nd fields
zzawk '{print $2, $1}' file.txt

# Move the 1st field to the end
zzawk '{first = $1; $1 = ""; sub(/^ /, ""); print $0, first}' file.txt
```

### Pattern 3: Print the last field (and second-to-last)

```shell
zzawk '{print $NF}' file.txt          # Last field
zzawk '{print $(NF-1)}' file.txt      # Second-to-last field
```

### Pattern 4: Change the field separator

```shell
zzawk -F: '{print $1}' /etc/passwd                # Input separated by ":"
zzawk -F, '{print $1, $3}' data.csv               # CSV input
zzawk 'BEGIN{FS=","; OFS="\t"} {$1=$1; print}' data.csv   # CSV -> TSV
```

### Pattern 5: Filter lines by field count

```shell
zzawk 'NF == 5' file.txt              # Lines with exactly 5 fields
zzawk 'NF > 3' file.txt               # Lines with more than 3 fields
zzawk 'NF != 4 {print NR": "$0}' data.txt   # Find malformed rows
```

### Pattern 6: Normalize whitespace

```shell
# Collapse runs of spaces/tabs into a single space, trim both ends
zawk '{$1=$1; print}' file.txt
```

> Assigning `$1=$1` forces zawk to rebuild `$0` using `OFS`, which squeezes extra whitespace.

## Pattern Matching and Filtering — How Do You Select Lines?

### Pattern 7: Match lines by string or regex

```shell
zawk '/error/' app.log                       # Lines containing "error"
zawk '/^#/' config.conf                      # Lines starting with "#"
zawk '!/^#/' config.conf                     # Lines NOT starting with "#"
zawk 'tolower($0) ~ /error|warn/' app.log    # Case-insensitive match
```

### Pattern 8: Filter by a field's value

```shell
zawk '$3 > 100' data.txt                     # 3rd field greater than 100
zawk '$1 == "admin"' users.txt               # 1st field equals "admin"
zawk '$2 ~ /^192\.168\./' access.txt         # 2nd field matches a regex
```

### Pattern 9: Combine conditions (AND / OR)

```shell
zawk '$3 > 100 && $4 == "OK"' data.txt       # AND
zawk '$1 == "GET" || $1 == "POST"' req.txt   # OR
zawk '!($5 == "200")' access.txt             # NOT
```

### Pattern 10: Skip empty lines

```shell
zawk 'NF' file.txt                   # Drop empty and whitespace-only lines
zawk 'length($0) > 0' file.txt       # Drop only truly empty lines
```

## Line Numbers and Ranges — How Do You Work with Line Positions?

### Pattern 11: Print a specific line or range

```shell
zawk 'NR == 10' file.txt             # Line 10 only
zawk 'NR >= 10 && NR <= 20' file.txt # Lines 10 through 20
zawk 'NR == 10 {print; exit}' big.txt   # Stop early on large files
```

### Pattern 12: Skip the header row

```shell
zawk 'NR > 1' data.csv               # Skip the first line
zawk 'NR > 1 {print $2}' data.csv    # Skip header, then extract a column
```

### Pattern 13: Odd or even lines

```shell
zawk 'NR % 2 == 1' file.txt          # Odd lines
zawk 'NR % 2 == 0' file.txt          # Even lines
```

### Pattern 14: Add line numbers

```shell
zawk '{print NR, $0}' file.txt               # Plain numbering
zawk '{printf "%04d %s\n", NR, $0}' file.txt # Zero-padded numbering
zawk 'NF {print ++n": "$0}' file.txt         # Number only non-empty lines
```

### Pattern 15: Print lines between two patterns

```shell
zawk '/START/,/END/' file.txt                # Inclusive range
zawk '/START/{f=1; next} /END/{f=0} f' file.txt   # Exclude the markers
```

## Aggregation — How Do You Compute Totals and Statistics?

### Pattern 16: Sum and average a column

```shell
zawk '{sum += $3} END {print sum}' data.txt
zawk '{sum += $3} END {if (NR > 0) print sum / NR}' data.txt
zawk 'NR > 1 {s += $2; n++} END {printf "avg=%.2f\n", s/n}' data.csv
```

### Pattern 17: Find max and min

```shell
zawk 'NR == 1 || $3 > max {max = $3} END {print max}' data.txt
zawk 'NR == 1 || $3 < min {min = $3} END {print min}' data.txt
# Print the whole line with the max value
zawk 'NR == 1 || $3 > max {max = $3; line = $0} END {print line}' data.txt
```

### Pattern 18: Count lines

```shell
zawk 'END {print NR}' file.txt               # Total lines (like wc -l)
zawk '/error/ {n++} END {print n+0}' app.log # Matching lines
```

> `n+0` prints `0` instead of an empty string when nothing matched.

### Pattern 19: Count occurrences per value (histogram)

```shell
zawk '{count[$1]++} END {for (k in count) print k, count[k]}' access.log
# Sum a value per key (group by)
zawk '{total[$1] += $2} END {for (k in total) print k, total[k]}' sales.txt
```

### Pattern 20: Count unique values

```shell
zawk '!seen[$1]++ {n++} END {print n}' access.log
```

### Pattern 21: Remove duplicate lines (preserve order)

```shell
zawk '!seen[$0]++' file.txt          # Unlike sort -u, keeps original order
zawk '!seen[$2]++' file.txt          # Deduplicate by the 2nd field
zawk 'x[$0]++ { i++ } END { print i }' file.txt
```

## String Manipulation — How Do You Replace and Reshape Text?

### Pattern 22: Replace text with sub / gsub

```shell
zawk '{sub(/foo/, "bar"); print}' file.txt    # First match on each line
zawk '{gsub(/foo/, "bar"); print}' file.txt   # All matches on each line
zawk '{gsub(/[ \t]+$/, ""); print}' file.txt  # Strip trailing whitespace
```

### Pattern 23: Replace only within a specific field

```shell
zawk '{gsub(/-/, "/", $2); print}' file.txt   # Only touch the 2nd field
zawk 'BEGIN{FS=OFS=","} {gsub(/"/, "", $3); print}' data.csv
```

### Pattern 24: Change case

```shell
zawk '{print toupper($0)}' file.txt
zawk '{print tolower($1)}' file.txt
# Capitalize the first letter of each line
zawk '{print toupper(substr($0,1,1)) substr($0,2)}' file.txt
```

### Pattern 25: Extract substrings and filter by length

```shell
zawk '{print substr($0, 1, 10)}' file.txt     # First 10 characters
zawk '{print substr($1, 3)}' file.txt         # From the 3rd character on
zawk 'length($0) > 80' source.txt             # Lines longer than 80 chars
zawk 'match($0, /[0-9]+/) {print substr($0, RSTART, RLENGTH)}' file.txt
```

## Formatted Output — How Do You Produce Clean Tables?

### Pattern 26: Align columns with printf

```shell
zawk '{printf "%-15s %10s\n", $1, $2}' file.txt      # Left/right aligned
zawk '{printf "%-10s %8.2f\n", $1, $3}' prices.txt   # 2 decimal places
zawk 'BEGIN{printf "%-10s %5s\n", "NAME", "AGE"} {printf "%-10s %5d\n", $1, $2}' people.txt
```

### Pattern 27: Turn a CSV into an aligned table

```shell
zawk -i csv '{printf "%-12s %-12s %8s\n", $1, $2, $3}' data.csv
# Auto width: two passes over the same file
zawk -i csv 'NR==FNR {for (i=1;i<=NF;i++) if (length($i) > w[i]) w[i] = length($i); next}
         {for (i=1;i<=NF;i++) printf "%-*s ", w[i], $i; print ""}' data.csv data.csv
```

## Multiple Files — How Do You Combine and Compare Files?

### Pattern 28: Process multiple files, skipping each header

```shell
zawk 'FNR > 1' a.csv b.csv c.csv              # NR counts globally, FNR per file
zawk 'FNR == 1 && NR != 1 {next} {print}' *.csv   # Merge CSVs, keep one header
zawk 'FNR == 1 {print "==> " FILENAME " <=="} {print}' *.log
```

### Pattern 29: Lines in one file but not in another

```shell
# Lines in b.txt that are not in a.txt
zawk 'NR == FNR {seen[$0]; next} !($0 in seen)' a.txt b.txt
# Lines present in both files
zawk 'NR == FNR {seen[$0]; next} $0 in seen' a.txt b.txt
# Join two files on the 1st field
zawk 'NR == FNR {name[$1] = $2; next} {print $0, name[$1]}' users.txt orders.txt
```

### Pattern 30: Setup in BEGIN, summary in END

```shell
zawk 'BEGIN {FS=","; print "Report"; print "------"}
     NR > 1 {total += $3; n++}
     END {printf "rows=%d total=%.2f avg=%.2f\n", n, total, (n ? total/n : 0)}' sales.csv
# Pass shell variables into zawk with -v
zawk -v limit=100 '$3 > limit' data.txt
```

## Cheatsheet

| Task                     | One-liner                                      |
|--------------------------|------------------------------------------------|
| Print a column           | `zawk '{print $2}'`                             |
| Last field               | `zawk '{print $NF}'`                            |
| CSV column               | `zawk -F, '{print $3}'`                         |
| Skip header              | `zawk 'NR > 1'`                                 |
| Filter by value          | `zawk '$3 > 100'`                               |
| Sum a column             | `zawk '{s += $1} END {print s}'`                |
| Average                  | `zawk '{s += $1} END {print s/NR}'`             |
| Count per key            | `zawk '{c[$1]++} END {for (k in c) print k, c[k]}'` |
| Deduplicate (keep order) | `zawk '!seen[$0]++'`                            |
| Replace all              | `zawk '{gsub(/old/, "new"); print}'`            |
| Range between patterns   | `zawk '/START/,/END/'`                          |
| Diff two files           | `zawk 'NR==FNR {a[$0]; next} !($0 in a)' f1 f2` |
