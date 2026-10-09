zawk: AWK + stdlib + Rust
=====================================================

zawk is a small programming language for writing short programs processing textual data.
To a first approximation, it is an implementation of the [AWK](https://en.wikipedia.org/wiki/AWK) language;
many common Awk programs produce equivalent output when passed to zawk.

You might be interested in zawk if you want your scripts to handle escaped CSV/TSV like standard Awk fields,
or if you want your scripts to execute faster,
or if you want a standard AWK library to make life easy.

![AWK Stdlib](https://github.com/linux-china/zawk/blob/master/info/awk-stdlib.png?raw=true)

Features:

* CSV/TSV support by frawk
* High performance
* gawk mostly compatible
* A standard library: text, math, datetime, crypto, parser, encode/decode, ID, KV, SQLite/MySQL, Redis/NATS etc.
* i18n support: `length("你好Hello") # 7`, `substr("你好Hello", 1, 2) # 你好`
* Load awk script from URL
* awk file help support

The info subdirectory has more in-depth information on zawk:

* [Overview](https://github.com/linux-china/zawk/blob/master/info/overview.md): what frawk is all about, how it differs
  from Awk.
* [Types](https://github.com/linux-china/zawk/blob/master/info/types.md): A quick gloss on frawk's approach to types and
  type inference.
* [Parallelism](https://github.com/linux-china/zawk/blob/master/info/parallelism.md): An overview of frawk's parallelism
  support.
* [Benchmarks](https://github.com/linux-china/zawk/blob/master/info/performance.md): A sense of the relative performance
  of frawk and other tools when processing large CSV or TSV files.
* [Standard Library](https://github.com/linux-china/zawk/blob/master/info/stdlib.md): A standard library by zawk,
  including exciting functions that are new when compared with Awk.
* [FAQ](https://github.com/linux-china/zawk/blob/master/info/faq.md): FAQ about zawk.

zawk/frawk is dual-licensed under MIT or Apache 2.0.

# Installation

Mac with Homebrew:

```shell
$ brew install --no-quarantine linux-china/tap/zawk
$ sudo xattr -r -d com.apple.quarantine $(readlink -f $(brew --prefix zawk))
```

or install by [cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```shell
$ cargo binstall zawk
```

You will need to [install Rust](https://rustup.rs/). If you have not updated rust in a while,
run `rustup update nightly` (or `rustup update` if building using stable).

### Building Using Stable

frawk currently requires a nightly compiler by default. To compile frawk using stable,
compile without the `unstable` feature. Using `rustup default nightly`, or some other
method to run a nightly compiler release is otherwise required to build frawk.

### Building a Binary

With those prerequisites, cloning this repository and a `cargo build --release`
or `cargo [+nightly] install --path <zawk repo path>` will produce a binary that you can
add to your `PATH` if you so choose:

```
$ cd <zawk repo path>
$ cargo +nightly install --path .
```

zawk is now on [crates.io](https://crates.io/crates/zawk), so running
`RUSTFLAGS="-C target-feature=+aes,+sse2" cargo install zawk ` with the desired features should also work.

SIMD fast paths (SSE2/AVX2) are selected at runtime, so the default build runs on any CPU of the target
architecture. To additionally optimize the rest of the code for the CPU you are building on, use:

```
$ RUSTFLAGS="-C target-cpu=native" cargo install zawk
```

The resulting binary may crash with `SIGILL` on other machines, so do not distribute it.

# Bugs and Feature Requests

frawk has bugs, and many rough edges. If you notice a bug in frawk, filing an issue
with an explanation of how to reproduce the error would be very helpful. There are
no guarantees on response time or latency for a fix. No one works on frawk full-time.
The same policy holds for feature requests.

# Credits

Thanks to Eli Rosenthal's [frawk](https://github.com/ezrosent/frawk).
zawk is based on frawk. Without frawk, there would be no zawk. 
