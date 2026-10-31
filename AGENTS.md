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

Security:

- http is allowed 
- No taint check required to execute command, and it is supported by third sandbox tool.

Architecture: 

- BLOB in Database: not support now, and treated as empty string

Development environment:

All configuration are in the `.env`, and you can read it if necessary.

- redis: `redis://127.0.0.1:6379`
- nats: `nats://127.0.0.1:14222/topic1`
- mqtt: `mqtt://127.0.0.1:1883/topic1`
- smtp url: `smtp://127.0.0.1:3535`
- s3 objects: `s3://mj-artifacts/demo.csv`
- http testing: use `http://127.0.0.1:18080` httpbin service
- mysql: `mysql://root:123456@127.0.0.1:3306/test`
- postgresql: `postgres://postgres:123456@127.0.0.1:15432/demo_db`


database schema for mysql and postgresql:

```sql
create table people
(
    id   int,
    name varchar(255),
    age int
);

insert into people(id, name, age) values (1, 'Jackie', 48);
insert into people(id, name, age) values (2, 'Love', 48);
insert into people(id, name, age) values (3, 'Leo', 24);
insert into people(id, name, age) values (4, 'Lucas', 10);
insert into people(id, name, age) values (5, 'Laura', 8);
insert into people(id, name, age) values (11, 'test', 18);
```

Notes:

- `Z` is the alias of `target/release/zawk`
- DO NOT use `--release` option for `cargo test`