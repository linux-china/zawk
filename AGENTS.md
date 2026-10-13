# Project overview

zawk is a modern AWK language implementation with Rust, and it's modern AWK + stdlib + Rust.

Features:

* AWK/gawk mostly compatible
* More functions for data process
* High performance

Tech Stack:

- AWK parser: [LALRPOP](https://github.com/lalrpop/lalrpop) - LR(1) parser generator for Rust
- Compiler backend: Cranelift

Notes:

- `Z` is the alias of `target/release/zawk`