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
* gawk compatible
* A standard library: text, math, datetime, crypto, parser, encode/decode, ID, KV, SQLite/MySQL, Redis/NATS etc.
* i18n support: `length("你好Hello") # 7`, `substr("你好Hello", 1, 2) # 你好`
* Load awk script from URL

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
$ sudo xattr -r -d com.apple.quarantine $(readlink -f $(brew --prefix zawk))/bin/zawk
```

or install by cargo:

```shell
$ cargo install zawk
```

*Note: zawk uses some nightly-only Rust features.
Build [without the `unstable`](#building-using-stable) feature to build on stable.*

You will need to [install Rust](https://rustup.rs/). If you have not updated rust in a while,
run `rustup update nightly` (or `rustup update` if building using stable). If you would like
to use the LLVM backend, you will need an installation of LLVM 15 on your machine:

* See [this site](https://apt.llvm.org/) for installation instructions on some debian-based Linux distros.
  See also the comments on [this issue](https://github.com/ezrosent/frawk/issues/63) for docker files that
  can be used to build a binary on Ubuntu.
* On Arch `pacman -Sy llvm llvm-libs` and a C compiler (e.g. `clang`) are sufficient as of September 2022.
* `brew install llvm@15` or similar seem to work on macOS.

Depending on where your package manager puts these libraries, you may need to
point `LLVM_SYS_150_PREFIX` at the llvm library installation
(e.g. `/usr/lib/llvm-15` on Linux or `/opt/homebrew/opt/llvm@15` on macOS when installing llvm@15 via Homebrew).

**Attention**: Compare to Cranelift, binary with LLVM is bigger(almost 32M vs 8.5M).

### Building Without LLVM

While the LLVM backend is recommended, it is possible to build frawk only with
support for the Cranelift-based JIT and its bytecode interpreter. To do this,
build without the `llvm_backend` feature. The Cranelift backend provides
comparable performance to LLVM for smaller scripts, but LLVM's optimizations
can sometimes deliver a substantial performance boost over Cranelift (see the
[benchmarks](https://github.com/linux-china/zawk/blob/master/info/performance.md) document for some examples of this).

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
# Without LLVM
$ cargo +nightly install --path .
# With LLVM, but with other recommended defaults
$ cargo +nightly install --path . --no-default-features --features use_jemalloc,llvm_backend,allow_avx2,unstable
```

zawk is now on [crates.io](https://crates.io/crates/zawk), so running
`cargo install zawk` with the desired features should also work.

# Bugs and Feature Requests

frawk has bugs, and many rough edges. If you notice a bug in frawk, filing an issue
with an explanation of how to reproduce the error would be very helpful. There are
no guarantees on response time or latency for a fix. No one works on frawk full-time.
The same policy holds for feature requests.

# Credits

Thanks to Eli Rosenthal's [frawk](https://github.com/ezrosent/frawk).
zawk is based on frawk. Without frawk, there would be no zawk. 
