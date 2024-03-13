FAQ
===========

# Why to create zawk?

[frawk](https://github.com/ezrosent/frawk) is good tool created by Eli Rosenthal.
We just want to make AWK more powerful with standard library. `zawk = frawk + stdlib`.

Time flies, and we need a new Modern AWK to work with DuckDB, ClickHouse, S3, KV etc. for text processing.

# Why not just contribute to frawk?

frawk is a foundation to zawk for syntax, types, lex etc., 
and zawk focuses to make AWK more powerful with standard library. 
Now I'm not sure that developers will accept my changes to frawk, and zawk just experimental work: `zawk = AWK + stdlib`.

Frawk still good for text processing, embedded etc., 
and if possible I will contribute some work to frawk, for example:

* Upgrade to Rust 2021
* Upgrade to Clap 4.5
* Dependencies updated to latest
* gawk compatible: global variables(ENVIRON) and functions(datetime etc.)

# zawk will fix some bugs in frawk?

Yes. Eli Rosenthal had much less time over the last 1-2 years to devote to bug fixes and feature requests for frawk,
and I will try my best to fix bugs in frawk. 

# Any roadmap for zawk?

Now I'm not sure about the roadmap, but I will try my best to make zawk more powerful and easy to use.

* gawk compatible
* stdlib perfect
* performance optimization
* UX: Installation, Usage, Documentation, Examples etc.

# How to query Apache Parquet?

```shell
$ duckdb -c "COPY (select * from 'family.parquet') TO 'family.csv' (FORMAT CSV)"
```