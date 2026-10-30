---
title: Date & Time
sidebar_label: Date/Time
---

# Date & Time

`systime`, `strftime` and `mktime` follow gawk, with a much more forgiving parser.

## Current time and formatting

```awk
systime()                           # Unix time
strftime("%Y-%m-%d %H:%M:%S")       # now, local time
strftime("%Y-%m-%d", 0)             # a timestamp
strftime("%H:%M", 0, 1)             # in UTC (3rd argument)
strftime("%+")                      # ISO 8601 / RFC 3339
```

Format directives are those of [chrono](https://docs.rs/chrono/latest/chrono/format/strftime/index.html).

## Parsing

`mktime` accepts gawk's `"YYYY MM DD HH MM SS"` and [many common formats](https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats):

```awk
mktime("2012 12 21 0 0 0")
mktime("2019-11-29 08:08-08")
mktime("2024-04-27 17:07:25.684184848 +08:00")
mktime("1970 01 02 00 00 00", 1)      # 86400: parse as UTC
mktime("2024-01-01 10:00:00", 0, 8)   # 1704074400: UTC offset in hours
```

It returns `-1` if the text cannot be parsed, as gawk does.

`datetime()` breaks a time into parts: `year`, `month`, `monthday`, `hour`, `minute`, `second`, `yearday`,
`weekday`, `althour`.

```awk
datetime()["year"]                  # now
datetime(1621530000)["month"]
datetime("2020-02-02")["weekday"]
```

## Date arithmetic

`date_add(time, n, unit)` and `date_diff(unit, start, end)` work on Unix timestamps in local time:

```awk
date_add(mktime("2024-01-31 10:00:00"), 1, "month")          # 2024-02-29 10:00:00: month ends are clamped
date_add(systime(), -7, "day")                               # a week ago
date_diff("day", mktime("2024-01-01"), mktime("2024-03-01"))  # 60
date_diff("month", mktime("2024-01-31"), mktime("2024-02-28"))  # 0: only complete units count
```

`date_diff` counts complete units from `start` to `end` (negative if `end` is earlier), as MySQL's `TIMESTAMPDIFF`.
Units: `second`/`s`, `minute`/`m`/`min`, `hour`/`h`, `day`/`d`, `week`/`w`, `month`/`mo`, `quarter`/`q`, `year`/`y`.
Days and weeks follow the wall clock, so a day across a DST change is 23 or 25 hours.

`date_trunc(unit, time)` truncates a timestamp to the start of its unit in local time, handy for grouping:

```awk
strftime("%F %T", date_trunc("hour", mktime("2024-02-15 10:37:42")))  # 2024-02-15 10:00:00
strftime("%F", date_trunc("week", mktime("2024-02-15")))              # 2024-02-12: weeks start on Monday
{ sales[strftime("%Y-%m", date_trunc("month", mktime($1)))] += $2 }   # monthly totals
```

`to_iso8601(time)` formats a timestamp as ISO 8601 in UTC, as JavaScript's `toISOString`:

```awk
to_iso8601(0)          # 1970-01-01T00:00:00.000Z
to_iso8601(systime())  # e.g. 2024-02-15T02:37:42.000Z
```

## Durations

```awk
duration("2min + 12sec")    # 132 seconds
duration("1 day")           # 86400
```

Units: `sec`, `min`, `hour`/`h`, `day`/`d`, `week`/`wk`, `month`/`mo`, `year`/`yr`.

## Example: orders per day

```shell
zawk -i csv -H '{ t = mktime($FI["created_at"]); c[strftime("%Y-%m-%d", t, 1)]++ }
  END { PROCINFO["sorted_in"] = "@ind_str_asc"; for (d, n in c) print d, n }' orders.csv
```
