# Parallelism in zawk

zawk provides a rudimentary model of parallelism that can speed up many simple 
scripts with little or no modifications. One relatively unique aspect of zawk's implementation is that it can achieve
nontrivial speedups even when the input is only a single input file or stream.
The first part of this doc explains the architecture that zawk uses to read
formats like CSV in a parallel-friendly manner. While simple aggregations do not need to be modified
to successfully run in parallel, running a script in
this mode _can_ change the meaning of a script. The second portion provides an
overview of the semantics of a zawk script when it is run in parallel.

> Note: parallel execution works with every input format (text, CSV, TSV,
> JSON/JSON Lines, Parquet, Avro, Excel, Markdown) and with any field separator,
> including multi-byte and regex ones, which are split in the workers. zawk runs
> the script serially instead, with a warning when it says nothing else, when:
>
> * the record separator `RS` is longer than one byte, or may be assigned at
>   run time (`-p r ignored: record separator '...' needs the regex splitter`);
> * `-H` is used with several input files (each file has its own header);
> * an operand is a `var=value` assignment, or the input is a `nats://` or
>   `kafka://` message stream.
>
> SIMD scanning of structural characters is used on x86_64 (SSE2/AVX2) and on
> aarch64 (NEON).

## Automatic Parallelism

Without `-p`, zawk runs a program as with `-pr` when doing so cannot change what
it prints:

* the input is local regular files (text, `-F`, CSV or TSV; not stdin or URLs)
  of 64 MiB or more in all;
* the main loop has no visible effect: no `print`/`printf`, `getline`,
  `system`, `exit`, user-defined function or range pattern, and only
  side-effect-free builtins (`length`, `substr`, `split`, `match`, `int`, ...);
* every variable the main loop writes is either a **whole-number sum**
  (`c++`, `a[$1]++`, `n += length($0)`, `s += int($3)`, `hits += $3 > 500`),
  not read in the main loop nor given a value in `BEGIN` or with `-v`, or a
  **per-record temporary** that each record assigns before reading it
  (`x = $3 * 2`, `n = split($2, parts)`, `for (i = 1; i <= NF; i++)`);
* the main loop does not read `NR`, `FNR`, `FILENAME` or `PID` (END may: it
  sees the values of a serial run), and END does not read the last record
  (`$0`, `$n`, `NF`).

For example `zawk '/ERROR/ { c++ } END { print c }' big.log` and
`zawk -F, '{ n[$2]++ } END { for (k in n) print k, n[k] }' big.csv` run in
parallel on their own. Sums of fractions such as `{ s += $3 }` stay serial:
the workers add in another order, which changes the last bits of the sum (and
may print `2.9997e+06` where the serial run prints `2999700`); add `-pr` to
run them in parallel anyway. `-p0` (or `ZAWK_PARALLEL=0`) turns automatic
parallelism off.

`for (k in a)` in END visits the keys of an array filled by the workers in an
order that depends only on the keys: the same from one run to the next, and
whatever `-j` or `--chunk-size`. It may differ from the order of a serial run
(AWK leaves the order unspecified); set `PROCINFO["sorted_in"]` (for example
`"@ind_str_asc"`) or pipe the output to `sort` for a given order.

## Reading Input In Parallel

Why expand the language in this way in the first place? After all, existing
tools like [GNU Parallel](https://www.gnu.org/software/parallel/) already
provide a succinct means of performing shell commands in parallel. I have 2
reasons for this, with reason 2 being more important than reason 1.

1. While existing solutions are quite succinct, some simple aggregations can be
   even easier to write in a single zawk program.
2. While it is relatively straightforward to run a command in parallel across
   multiple input files, existing tools have a hard time achieving parallelism
   _within a single input file_.

zawk supports a file-per-worker model of parallelism using the `-pf` option,
but I think its support for record-level parallelism (under the `-pr` option) is
more interesting.

Consider the CSV format. Parallelizing CSV parsing is a difficult task because a
parser must take a different action based on whether characters like commas
occur inside a quoted field. Most
[existing](https://github.com/BurntSushi/rust-csv/blob/master/csv-core/src/reader.rs)
solutions I have come across construct a state machine that a parser steps
through on a byte-by-byte basis.

Recent approaches to parsing [JSON](https://arxiv.org/abs/1902.08318) and
[CSV](https://github.com/geofflangdale/simdcsv) instead perform a first pass to
write out a sequence of locations of _structural characters_ within the input:
Relevant structural characters in CSV are `,`s that are not inside a quoted
field, `"` characters, `\n` and `\r\n` sequences. The remaining parsing task may
still need to step through a state machine of some kind, but this state machine
only executes once per structural character, not once per byte in the input.
Because this first phase can be implemented _extremely_ cheaply using SIMD
instructions, this approach achieves substantial end-to-end performance gains on
recent CPUs.

zawk implements this approach for scripts with CSV, TSV and
single-byte-separator inputs. Not only does this approach provide high
performance for all scripts that consume input in this form, the separation of
parsing into two phases provides us with an opportunity to parallelize the reading
of a single CSV file. A single worker thread performs an initial pass on a chunk
of input data to discover structural characters, it then locates a relevant
record separator and sends that chunk off to a worker thread. That worker thread
can then finish the parsing task at its own pace.

This architecture doesn't scale perfectly --- I've seen diminishing marginal
returns after 4-6 workers depending on the machine --- but it scales fast enough
to process CSV files at >2GB/s on my laptop, which is much faster than I have
been able to process CSV otherwise. The [performance
doc](https://github.com/linux-china/zawk/blob/master/info/performance.md) provides
measurements of the speedups that different zawk scripts achieve when run this
way, as well as comparisons to other tools performing the same task.

## The Meaning of Parallel zawk Programs

zawk supports a limited notion of parallelism suitable for performing simple
aggregations or transformations on textual data. The goal of zawk's parallelism
in its current form is to facilitate parallelizing zawk scripts that are
already embarrassingly parallel. While there are many possible directions to
go from here, this strikes me as a modest but useful first step.

Like Awk, zawk executes programs as a sequence of "patterns" and "actions."
Actions are blocks of code that are executed if a pattern matches. Most patterns
are tested against each successive line of input, with the exception being
the `BEGIN` and `END` patterns whose corresponding actions are executed before
and after any input is read, respectively.

> To be precise, Awk scripts that only have a `BEGIN` pattern never read any
> input outside of explicit `getline` calls.

When zawk is passed the `pr` or `pf` command-line options, it compiles the program
in _parallel mode_. In this mode, the zawk program is broken into three "stages":

1. The `BEGIN` block is executed by a single thread.
2. The main loop (i.e. pattern/action pairs aside from `BEGIN` and `END`) is
   executed independently in parallel by a configurable (via the `-j` flag) 
   number of worker
   threads. Variables mentioned in both the `BEGIN` block and the main loop are
   copied to the worker threads.
3. The `END` block is executed by a single thread after all worker threads
   terminate. Variables mentioned in both the `END` block and the main loop are
   copied from each worker thread and _aggregated_ before being accessed by the
   thread executing the `END` block. The record counters are combined too:
   `NR`, `FNR` and `FILENAME` in `END` have the values of a serial run (`FNR`
   and `FILENAME` describe the last input file, whose records several workers
   may have shared).

`exit` works as in serial mode: `exit` in `BEGIN` skips the main loop, and
`exit` in the main loop stops every worker. In both cases the `END` block still
runs (on the aggregated results), and the program then exits with the code of
`exit` (or of a later `exit code` in `END`). Workers stop at the next chunk of
input, not the next record, so records after the one that called `exit` may
still have been processed, and `NR` in `END` counts them:

```awk
/ERROR/ { found = 1; exit 1 }
END { print found ? "error found" : "ok" }
```

Here is a simple "select" script written in zawk that extracts the 2nd column
of an input source:
```awk
{ print $2; }
```

Running this script with the `-pr` option will run this
script in parallel, with a dynamic number of worker threads each getting a roughly equal portion of the input.
The output of the parallel script will be the same
as an invocation without the `-pr` option up to a reordering of the output rows.
A similar workload in the [performance
doc](https://github.com/linux-china/zawk/blob/master/info/performance.md) gets
close to a 2x speedup in record-oriented parallel mode, despite the fact that
writes to output files are all serialized, and all input records come from a
single file.

With `--keep-order` (which implies `-pr`, so `--keep-order -j 8` needs no `-p`)
the output is written in the order of the input, as a serial run writes it.
Workers that run ahead of a slow chunk (one calling `system()`, say) hold their
output back, and wait before taking more input once 64 MiB of output is held,
so memory stays bounded.

Some programs still depend on seeing every record in one thread, and print
something else in parallel mode, ordered or not:

* Record counters in the main loop: each worker counts its own records, so
  `NR % 1000000 == 0` or `NR == 1` match per worker (`W301`; a run with
  `--keep-order` prints it as a warning).
* Range patterns (`/BEGIN/, /END/`): each worker keeps its own range state
  (`W304`). With `--keep-order` such a program runs serially, with a warning.
* State carried from one record to the next that decides or makes output,
  such as deduplication (`!seen[$1]++`), comparing with the previous record
  (`$1 != prev`) or a running total (`{ n++; print n }`): each worker has its
  own copy, so a key is printed once per worker and the records at chunk
  boundaries see the wrong previous value (`W305`).

A run with `-p` (or `--keep-order`) prints `W304` and `W305` as warnings;
`zawk lint -p r` lists all of them. Run such programs serially (`-p0`).

### Aggregations

_Implicit Aggregations_ Variables that are referenced in both the main loop and
`END` block of a script are implicitly aggregated across all worker threads.
Scalars are aggregated according to rules that are a bit arbitrary, but as we
shall see you always have the option of performing the aggregation explicitly.

* Scalars are aggregated differently based on their
  [type](https://github.com/linux-china/zawk/blob/master/info/types.md). Integer
  and floating-point values are summed. String variables are aggregated by picking
  an arbitrary non-empty representative value from one of the worker threads.
* Maps are aggregated by performing a union of the underlying sets of key/value
  pairs, with overlapping values being aggregated according to the corresponding
  scalar rule.
* Every worker starts from a copy of the values the `BEGIN` block set, and these
  starting values are counted once, not once per worker: a number is aggregated
  as `begin + Σ (worker − begin)`, the same for each value of a map, and a
  string a worker did not change is not picked over one it did. A variable
  that only `BEGIN` sets keeps its value. So a counter or budget initialized in
  `BEGIN` gives the same result in serial and parallel mode:

```awk
BEGIN { budget = 1000 }
{ budget -= $3 }
END { print budget }
```

  This does not help with plain assignments in the main loop: in
  `BEGIN { t = 5 } { t = $1 } END { print t }` every worker's `t` is a value it
  assigned, and END sees their sum. `zawk lint -p r` reports such variables
  (`W302`).
  The same goes for array elements: with `{ a[$4] = 1 }` a key seen by three
  workers has the value 3 in END, and `{ last[$1] = $2 }` adds up the last
  values of each worker. The keys (`for (k in a)`, `length(a)`) are right;
  the values are not. Count with `a[k]++` / `a[k] += v`, or collect per-worker
  values in PREPARE. `zawk lint -p r` reports such arrays (`W303`).

This, among other things, means that simple aggregations like sums:
```awk
{ SUM += $1 }
END { print SUM }
```
Or group-by's:
```awk
{ HIST[$1]++ }
END {
    for (k in HIST) {
        print k, HIST[k];
    }
}
```
Produce equivalent output when run in serial and parallel modes, with the usual
caveats about map iteration ordering (undefined) and floating point addition
(it is not associative).

_Other Aggregations_ While useful, the aggregations that happen by default
are not universally applicable. Consider the embarrassingly parallel task of
finding the maximum (lexicographic) value of a particular column:

```awk
{
    if (NR==1) {
        max=$2;
    } else {
        max=max>=$2?max:$2;
    }
}
END {
    print max;
}
```

This script is no longer correct if it is run in parallel. In parallel, the
aggregation rules dictate that it will simply return _a_ maximum value observed
by one of the worker threads. To aggregate explicitly, worker threads are
provided with a `PID` variable which takes on a positive integer value counting
up from 1, with each thread receiving a unique `PID`. Note that `PID` values
aren't always consecutive.  This, combined with the implicit aggregation for
maps, lets us write an _explicit_ max aggregation.

```awk
{
    if (NR==1) {
        max[PID]=$2;
    } else {
        max[PID]=max[PID]>=$2?max[PID]:$2;
    }
}
END {
    for (pid in max) {
        if (!i++) {
            max_val = max[pid]
        } else {
            max_val = max_val>max[pid]?max_val:max[pid]
        }
    }
    print max_val;
}
```

Because the repeated map references are both annoying to write and inefficient
to execute, zawk has a `PREPARE` block which executes in the worker threads at
the end of its input:

```awk
{
    if (NR==1) {
        max=$2;
    } else {
        max=max>=$2?max:$2;
    }
}
PREPARE { max_map[PID] = max }
END {
    for (pid in max_map) {
        v = max_map[pid]
        max = max>v?max:v
    }
    print max;
}
```

For a more involved example of an explicit aggregation, see the "Statistics"
benchmark in the [performance
doc](https://github.com/linux-china/zawk/blob/master/info/performance.md).
