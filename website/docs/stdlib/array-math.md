---
title: Arrays, Statistics & Math
sidebar_label: Array & Math
---

# Arrays, Statistics & Math

## Building and sorting arrays

```awk
s = seq(5)                 # 1..5, indexed from 1 (like the seq command)
s = seq(0, 2, 10)          # 0, 2, 4, 6, 8, 10
u = uniq(arr)              # unique values, sorted; uniq(arr, "keep") keeps first-seen order
n = asort(arr)             # sort values in place
n = asorti(count, keys)    # sorted indices into keys; how: "@ind_num_desc", "@val_num_asc", ...
print _join(arr, ",")
print length(arr)
delete arr[1]; delete arr
```

## Statistics

Exact statistics over the values of any array (strings are converted to numbers; 0 for an empty array):

| Function | Description |
|----------|-------------|
| `_sum`, `_mean`, `_min`, `_max` | sum, mean, min, max |
| `_median(arr)` | median |
| `_quantile(arr, q)` | q-quantile, `0 <= q <= 1`, linear interpolation (R type 7, numpy) |
| `_var`, `_stddev` | sample variance and standard deviation |
| `_mode(arr)` | most frequent value |
| `_count_distinct(arr)` | number of distinct values |
| `_top(arr, k)` | the `k` keys with the largest values |

```shell
# p50/p99 latency from an access log
zawk '{ lat[NR] = $NF } END { print _median(lat), _quantile(lat, 0.99), _stddev(lat) }' access.log

# Top 10 client IPs
zawk '{ c[$1]++ } END { t = _top(c, 10); for (i = 1; i <= length(t); i++) print t[i], c[t[i]] }' access.log
```

## Bloom filter

Constant-memory duplicate detection for huge inputs:

```awk
bf_insert(item); bf_insert(item, "group")
bf_contains(item)
bf_icontains(item)          # insert if not found, return whether it was found
```

```shell
zawk '!bf_icontains($0)' huge.txt     # dedupe without keeping every line in memory
```

## Math

POSIX functions (`sin`, `cos`, `atan2`, `exp`, `log`, `sqrt`, `int`, `rand`, `srand`) are there, plus:

```awk
abs(-1); floor(4.5); ceil(4.5); round(4.4)
log2(8); log10(100)
min(1, 2, 3); max("A", "B")
intdiv0(7, 0)              # 0 instead of a fatal division by zero
float("11.2"); int("11")
eval("a + 2", ctx)         # evaluate an expression with variables from the array ctx
fend("1 GiB to MB")        # unit-aware calculator, see https://github.com/printfn/fend
```

Bitwise: `and`, `or`, `xor`, `compl`, `lshift`, `rshift`, `rshiftl`.
