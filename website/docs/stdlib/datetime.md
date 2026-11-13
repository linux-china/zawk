---
title: Date & Time
sidebar_label: Date/Time
---

# Date & Time

`systime`, `strftime` and `mktime` follow gawk, with a much more forgiving parser.

## Current time and formatting

```awk
systime()                           # Unix time
systime_ms()                        # Unix time in milliseconds
strftime("%Y-%m-%d %H:%M:%S")       # now, local time
strftime("%Y-%m-%d", 0)             # a timestamp
strftime("%H:%M", 0, 1)             # in UTC (3rd argument)
strftime("%H:%M %Z", 0, "Asia/Shanghai")   # "08:00 CST": in an IANA time zone
strftime("%+")                      # ISO 8601 / RFC 3339
```

A 3rd argument that is a number or `"true"` means UTC; a signed UTC offset (`"+08:00"`, `"-0500"`, `"+08"`) is that fixed
offset; any other text must be an IANA time zone name, and a misspelt one (`"Asia/Shangai"`, or `"08:00"` without a sign)
falls back to UTC with a warning and `ERRNO` set.

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

When the format is known, `strptime(s, format[, tz])` parses with a [chrono format](https://docs.rs/chrono/latest/chrono/format/strftime/index.html)
and guesses nothing: `"01/02/2024"` is January 2 with `"%m/%d/%Y"`, February 1 with `"%d/%m/%Y"`. Text with `%z` or `%s`
is an instant; other text is local time, or in `tz`: an IANA time zone name, a signed UTC offset (`"+08:00"`, `"-0500"`), or UTC for a number or `"true"` (an unknown zone name is UTC with a warning and `ERRNO` set).
It returns `-1` if the text does not match the format.
A zone abbreviation (`%Z`) is ambiguous (`CST` is China, US Central or Cuba), so it is only taken when it is
`UTC`/`GMT`/`UT`/`Z` or the abbreviation `tz` uses at that time; any other name gives `-1` with a warning.

```awk
strptime("01/02/2024", "%d/%m/%Y")                          # local midnight of February 1
strptime("2024-01-02 08:00", "%Y-%m-%d %H:%M", "Asia/Shanghai")  # 1704153600
strptime("02/Jan/2024:08:00:00 +0800", "%d/%b/%Y:%H:%M:%S %z")   # 1704153600
strptime("2024-01-02 08:00 CST", "%Y-%m-%d %H:%M %Z", "Asia/Shanghai")  # 1704153600
```

`datetime()` breaks a time into parts: `year`, `month`, `monthday`, `hour`, `minute`, `second`, `yearday`,
`weekday`, `althour`, `quarter` (1-4), `month_lastday` (last day of the month, 28-31), `week` (ISO 8601 week, same as `strftime("%V")`) and `isoyear`
(ISO 8601 week-based year, same as `strftime("%G")`).

```awk
datetime()["year"]                  # now
datetime(1621530000)["month"]
datetime("2020-02-02")["weekday"]
datetime("2021-01-01")["week"]      # 53: belongs to the last ISO week of 2020
datetime("2024-02-10")["month_lastday"]  # 29
```

## Date arithmetic

`date_add(time, n, unit)` and `date_diff(unit, start, end)` work on Unix timestamps in local time:

```awk
date_add(mktime("2024-01-31 10:00:00"), 1, "month")          # 2024-02-29 10:00:00: month ends are clamped
date_add(systime(), -7, "day")                               # a week ago
date_diff("day", mktime("2024-01-01"), mktime("2024-03-01"))  # 60
date_diff("month", mktime("2024-01-31"), mktime("2024-02-28"))  # 0: only complete units count
date_diff("day", "2024-01-01", "2024-03-01")                 # 60: date text is parsed as mktime does
```

`date_diff` counts complete units from `start` to `end` (negative if `end` is earlier), as MySQL's `TIMESTAMPDIFF`.
Units: `second`/`s`, `minute`/`m`/`min`, `hour`/`h`, `day`/`d`, `week`/`w`, `month`/`mo`, `quarter`/`q`, `year`/`y`.
Days and weeks follow the wall clock, so a day across a DST change is 23 or 25 hours.

`date_trunc(unit, time)` truncates a timestamp to the start of its unit in local time, handy for grouping:

```awk
strftime("%F %T", date_trunc("hour", mktime("2024-02-15 10:37:42")))  # 2024-02-15 10:00:00
strftime("%F", date_trunc("week", mktime("2024-02-15")))              # 2024-02-12: weeks start on Monday
{ sales[strftime("%Y-%m", date_trunc("month", $1))] += $2 }   # monthly totals, $1 parsed as mktime
```

`to_iso8601(time)` formats a timestamp as ISO 8601 in UTC, as JavaScript's `toISOString`. The time is in whole seconds (a fraction is dropped), so the milliseconds are always `.000`:

```awk
to_iso8601(0)          # 1970-01-01T00:00:00.000Z
to_iso8601(systime())  # e.g. 2024-02-15T02:37:42.000Z
to_iso8601("2024-02-15 10:37:42 +08:00")  # 2024-02-15T02:37:42.000Z
to_iso8601(1.9)        # 1970-01-01T00:00:01.000Z
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
