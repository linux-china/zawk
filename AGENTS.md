# Project overview

zawk is a modern AWK language implementation with Rust, and it's modern AWK + stdlib + Rust.

Features:

* AWK/gawk mostly compatible
* More functions for data process
* High performance

Tech Stack:

- AWK parser: [LALRPOP](https://github.com/lalrpop/lalrpop) - LR(1) parser generator for Rust
- Compiler backend: Cranelift
- Apache OpenDAL: operator for object storage, file systems

Please ignore the following differences with gawk：

-  `BEGINFILE` / `ENDFILE`， `next`/`nextfile`
- `FIELDWIDTHS` / `FPAT`
- `@include "nonexistent"`
- `RT`
-  readonly
- `a[i][j]` 
- ignore f-string difference
- readonly variables `$0`, `$1`, `NF`, `NR`, `ARGV`/`ARGC`, just lint not syntax error
-  `$$1`, `$++i` not support
-  `@/re/` 

Notes:

- `Z` is the alias of `target/release/zawk`