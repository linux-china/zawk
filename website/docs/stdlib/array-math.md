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
abs(-1); floor(4.5); ceil(4.5); round(4.4); round(3.14159, 2)
log2(8); log10(100)
min(1, 2, 3); max("A", "B")
intdiv0(7, 0)              # 0 instead of a fatal division by zero
float("11.2"); int("11")
eval("a + 2", ctx)         # evaluate an expression with variables from the array ctx
fend("1 GiB to MB")        # unit-aware calculator, see https://github.com/printfn/fend
```

Bitwise: `and`, `or`, `xor`, `compl`, `lshift`, `rshift`, `rshiftl`.

## Exact decimals (money)

awk numbers are 64-bit floats: `0.1 + 0.2` is `0.30000000000000004`, `round(2.675, 2)` is `2.67`, and a sum over
millions of amounts drifts. The `dec_*` functions take decimal text and return decimal text, computing exactly in
between (28 significant digits):

| Function | Result |
|----------|--------|
| `dec_add("0.1", "0.2")` | `0.3` |
| `dec_sub("1.00", "0.99")` | `0.01` |
| `dec_mul("19.99", 3)` | `59.97` (the scale is the sum of the scales: `dec_mul("1.10", "1.10")` is `1.2100`) |
| `dec_div(10, 3, 2)` | `3.33`; without a scale, 28 significant digits with trailing zeros removed |
| `dec_round("2.675", 2)` | `2.68`; `dec_round(3, 2)` is `3.00`, `dec_round("1250", -2)` is `1300` |
| `dec_format("1234567.5", 2)` | `1,234,567.50` |
| `dec_cmp("1.10", "1.1")` | `0`; `-1` or `1` when the first is smaller or greater |

`dec_div`, `dec_round` and `dec_format` take an optional rounding mode after the scale: `half_up` (default, as Excel
`ROUND`), `half_even` (banker's rounding), `half_down`, `up`, `down`, `ceil`, `floor`.

```shell
# Revenue per customer, with 13% tax, exact to the cent
zawk -i csv -H '
  { total[$FI["customer"]] = dec_add(total[$FI["customer"]], dec_mul($FI["price"], $FI["qty"])) }
  END {
    for (c in total) print c, total[c], dec_round(dec_mul(total[c], "0.13"), 2)
  }' orders.csv
```

- Inputs may have blanks, a sign, an exponent (`1.5e3`) and thousands separators (`1,234.50`, `1_234.50`); an empty
  string is 0, so accumulators need no initialization.
- Invalid text, a division by zero and an overflow return `""` (`0` for `dec_cmp`) with a warning, and set `ERRNO`.
- Pass fields or strings (their original text), not float expressions. A float argument is converted to its shortest
  round-trip text (not with `CONVFMT`), so `dec_add(x, 0)` with `x = 1234567.89` is `1234567.89`; but arithmetic before
  the call is binary floating point: `dec_add(0.1 + 0.2, 0)` is `0.30000000000000004`. Write
  `dec_add(t, dec_mul($3, "1.06"))`, not `dec_add(t, $3 * 1.06)`; `zawk lint` warns about the latter (W123). Compare
  amounts with `dec_cmp`, not `<`, when they have different scales or come from different sources.
