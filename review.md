# zawk 代码 Review（2026-11-19）

## 1. 结论

- 这次 review 只读了代码，**没有编译，也没有运行测试**（按 AGENTS.md 的要求）。gawk 的行为用本机的 GNU Awk 5.1.0 核对过。
- 发现 **2 个中等问题**（结果静默出错）、**3 个中低问题**和若干低优先级问题，见第 3 节。
- 最近几次提交（getline 求值顺序、`to_csv` 加引号、TSV `\r`、`builtin_assigns`）的修复方向都对，但其中一个没修全：`asort`/`asorti` 漏掉了（问题 1，已修复）。问题 2～10 也已修复。
- 自动并行判定（`auto_parallel.rs`）、`printf`、`decimal`、`date_time`、文件描述符池/写入器淘汰、`array_stats` 这几块逐段读过，**没有发现逻辑错误**。

| # | 级别 | 问题 | 位置 | 状态 |
|---|---|---|---|---|
| 1 | 中 | `a[length(d)] = asort(s, d)` 先求下标，与 gawk 相反 | `src/cfg.rs:3693` | 已修复 |
| 2 | 中 | `-i csv` 下重新赋值 `$0` 时，字段中间的 `"` 被当成引号开头，字段被合并 | `src/runtime/splitter/batch.rs:552` | 已修复 |
| 3 | 中低 | `system()`、`cmd \| getline`、`print \| cmd` 的命令行按 lossy UTF-8 转换，非 UTF-8 字节变成 U+FFFD | `src/runtime/command.rs:63` | 已修复 |
| 4 | 中低 | `system2()` 执行前没有刷新输出，命令读不到刚 `print >` 的内容 | `src/interp.rs:3482`、`src/codegen/intrinsics.rs:1044` | 已修复 |
| 5 | 中低 | `http_post` 的 body 按 lossy UTF-8 转换，二进制或 GBK body 被破坏 | `src/runtime/network.rs:287` | 已修复 |
| 6 | 低 | 数据库参数数组有空洞时，后面的参数会前移 | `src/runtime/sqlite.rs:73` | 已修复 |
| 7 | 低 | `-o parquet` 不处理重复列名（`-o avro`、`-o jsonl` 会处理） | `src/runtime/parquet_writer.rs:323` | 已修复 |
| 8 | 低 | `system()` 启动失败时返回 errno 值 | `src/runtime/command.rs:84` | 已修复 |
| 9 | 低 | `left`/`right`/`translate`/`reverse`/`trim` 不受 `-b` 影响 | `src/runtime/text_funcs.rs:113` | 已修复 |
| 10 | 低 | 对含非法 UTF-8 的字符串，`printf` 的宽度和 `length()` 计数方式不同 | `src/runtime/printf.rs:361` | 已修复 |

## 2. 范围与方法

- **重点阅读**：
  - `cfg.rs`：求值顺序、`may_assign`、getline 的展开
  - `auto_parallel.rs`
  - `runtime/printf.rs`、`string_util.rs`、`text_funcs.rs`、`math_util.rs`、`decimal.rs`、`date_time.rs`、`array_stats.rs`、`sorted_in.rs`、`csv.rs`、`json.rs`、`sqlite.rs`、`kv.rs`、`network.rs`、`command.rs`
  - `str_impl.rs` 的 `sub_str`、字符游标、`trim_sides`、`as_text`/`from_text`
  - `splitter/regex.rs`、`splitter/batch.rs`（CSV/TSV 的拆分）、`writers.rs`（FdPool 淘汰和重新打开）、`pushdown.rs`、`include.rs`
- **查漏补缺**：逐个检查最近的 fix 提交（`a68f9f3`、`e272765`、`9cec901`、`c4bfc9a`、`0d11adb`），看修复是否覆盖了同类情况。
- **对照 gawk**：凡是涉及兼容性的结论，都先用 gawk 5.1.0 跑过，例如 `substr` 的非整数参数、`cmd | getline` 与 NR、`asort` 的求值顺序。
- 按 AGENTS.md 忽略的差异（`BEGINFILE`、`FPAT`、`RT`、`$0` 只读等）和 `tests/compat/known_failures.txt` 已经登记的差异都没有再列出。

## 3. 发现的问题

### 问题 1（中）：`builtin_assigns` 漏掉 `asort`/`asorti`，赋值时先求了下标

**位置**：`src/cfg.rs:3693`（`builtin_assigns`），由 `may_assign` 调用（`src/cfg.rs:3659`）

提交 `a68f9f3` 把 `match`、`split`、`patsplit`、`rand`、`srand` 算作“有副作用的内建函数”，于是 `a[RSTART] = match(...)` 会先求值、再求下标。但 `asort(src, dst)` 和 `asorti(src, dst)` 同样会改写数组（`dst`，只有一个参数时改写 `src` 本身），却不在列表里。结果 `may_assign` 返回 false，`Assign(Index(..))` 仍然先求下标。

**复现**（gawk 5.1.0 的输出是 `3 3`）：

```awk
BEGIN {
    s[1] = "b"; s[2] = "a"; s[3] = "c"
    a[length(d)] = asort(s, d)     # gawk: a[3] = 3；zawk: a[0] = 3
    for (k in a) print k, a[k]
}
```

`a[d[1]] = asort(s, d)` 同理：gawk 写入 `a["a"]`，zawk 写入 `a[""]`。`asorti` 也一样。`print d[1], asort(s, d)` 这类左到右求值的场景依赖 `snapshot_before`，也会受影响。

**修复建议**：在 `builtin_assigns` 里加上 `Asort | Asorti`，并在 `tests/compat/cases/03-evaluation-order-builtins.awk` 里补一个用例。

**状态**：已修复（未编译、未运行测试）。

- `src/cfg.rs` 的 `builtin_assigns` 加上了 `Asort | Asorti`，`may_assign` 和 `snapshot_before` 因此都会先求值、再求下标。
- 注释写明了两类有意不列入的情况：只在失败时设置 `ERRNO` 的 stdlib 函数，以及只读取数组参数的函数（`http_get`/`http_post`、`jwt`、SQL 函数）。
- `tests/compat/cases/03-evaluation-order-builtins.awk` 补了 5 个用例：`asort` 两个参数（`length(d)` 和 `d[1]` 作下标）、`asort` 一个参数、`asorti` 两个参数（`length(w)` 和 `w[1]` 作下标）。`.out` 用 gawk 5.1.0 重新生成。

另外两点可以顺便确认：

- 会设置 `ERRNO` 的 stdlib 函数（失败时调用 `stdlib_warning`）理论上也有副作用，例如 `a[ERRNO] = read_all("missing")`。这种用法很少见，可以不处理，但最好在注释里写明。
- 实际改写数组参数的只有 `asort`/`asorti`。`array_params()` 里的 `HttpGet`、`HttpPost`、`Jwt` 和 SQL 函数只读取数组，不需要加。

### 问题 2（中）：`-i csv` 下 `$0` 重新拆分的规则与读取输入不一致

**位置**：`src/runtime/splitter/batch.rs:552`（`split_record`），由 `Line::set_col(0, ..)` 调用（`batch.rs:518`）

主输入读取器（`csv_lenient_scan` 和 SIMD 扫描）按 gawk 的规则处理引号：

- 只有**字段开头**的 `"` 才开始一个带引号的字段；
- 带引号的字段内部，`"` 后面跟的不是 `"`、`,`、`\n` 或 `\r\n` 时，这个 `"` 是普通字符。

`split_record` 的注释说它用“the same rules as Stepper::step”，但实际实现是：

- **任何位置**的 `"` 都会开始一段引号内容（`(InputFormat::CSV, b'"')` 分支没有检查是否在字段开头）；
- 引号内容里，任何“后面不是 `"`”的 `"` 都会结束引号。

后果是：同一条记录，读进来时拆得正确，但只要 `$0` 被重新赋值，字段就会被合并，引号也会丢失。很多常见写法都会给 `$0` 重新赋值，例如 `$0 = $0`、`sub(/x/, "y")`、`gsub(...)`（对象是 `$0`）、`$0 = tolower($0)`。

**复现**：

```sh
printf 'id,size 5" screen,price\n' | zawk -i csv '{ print NF; sub(/id/, "ID"); print NF, $2 }'
# 读入时：NF=3，$2 = size 5" screen
# sub 之后：split_record 把 `" screen,price` 当成引号内容，
#           得到 NF=2，$2 = size 5 screen,price（引号丢失，字段合并）
```

带引号的字段内部出现普通引号时也一样，例如 `"say "hi" now",x`：两边的拆分结果不同。

**修复建议**：让 `split_record` 直接复用 `csv_lenient_scan` 和 Stepper 的逻辑，至少要加上“只在字段开头才开始引号”和“引号内 `"` 后面跟其他字符时是普通字符”这两条规则。再补一个单元测试：对同一批不规范的 CSV 记录，断言“读入后的字段”和“`$0 = $0` 之后的字段”相同。现有测试（`batch.rs:4430`）只用了规范的引号字段，覆盖不到这种情况。

**状态**：已修复（未编译、未运行测试）。

- `src/runtime/splitter/batch.rs` 的 `split_record` 改为按 `csv_lenient_scan` + `Stepper::step` 的规则拆分 CSV：用 `field_start` 记录是否在字段开头，只有字段开头的 `"` 才进入带引号的字段，其他位置的 `"` 原样保留；带引号的字段里，`""` 是转义的引号，`"` 后面是 `,`、`\n`、`\r\n` 或结尾时结束字段，后面是其他字符时是普通引号（原样保留，继续在引号内）。TSV 的处理不变。
- 唯一有意保留的差异写进了注释：读取时，普通引号之后（按奇偶在引号外）遇到换行会结束记录；`split_record` 拆的是单个 `$0`，这种换行只可能来自对 `$0` 的赋值，这里把它留在字段里。
- 新增单元测试 `csv_reassigned_record_splits_like_input`：13 条记录（字段中间的引号、带引号字段里的普通引号、`""` 转义、引号内的换行和 `\r\n`、空字段等），对每条记录分别以 `\n` 和 `\r\n` 结尾读入，断言读入的字段与 `split_record` 的结果相同；再把所有记录拼成一个输入，用 512/7/3 三种块大小读入（经过 SIMD 扫描切换到 lenient 扫描的路径）做同样的断言。另外直接断言了复现用例：`id,size 5" screen,price` 拆成 `id`、`size 5" screen`、`price`，`"say "hi" now",x` 拆成 `say "hi" now`、`x`。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 3（中低）：命令行按 lossy UTF-8 转换

**位置**：

- `src/runtime/command.rs:63`：`prepare_command(prog: &str, ..)`
- `command.rs:138`、`command.rs:197`：`String::from_utf8_lossy(bs)`
- `interp.rs:3479`、`intrinsics.rs:1040`：`run_command(&cmd.as_str())`

`system()`、`cmd | getline`、`print | cmd`、`system2()` 的命令字符串都先经过 `from_utf8_lossy` 或 `Str::as_str()`，再交给 `sh -c`。命令里的非 UTF-8 字节（Latin-1 或 GBK 编码的文件名）会被替换成 U+FFFD，命令就会作用到另一个文件名上，或者找不到文件。

而输出重定向的文件名走的是 `bytes_path`（无损），ENVIRON 走的是 `env_os_string`（无损）。只有命令行这一处是有损的，行为不一致。gawk 会原样传递字节。

```sh
printf 'caf\xe9.txt\n' | zawk '{ system("ls -l " $0) }'   # 实际执行的是 ls -l caf�.txt
```

**修复建议**：Unix 下让 `prepare_command` 接收 `&[u8]`，用 `OsStr::from_bytes` 构造参数，`Command::new("sh").arg("-c").arg(os_str)`。非 Unix 平台保留 lossy 转换。

**状态**：已修复（未编译、未运行测试）。

- `src/runtime/command.rs`：`prepare_command` 改为接收 `&[u8]`。新增 `command_line`：Unix 下用 `OsStr::from_bytes` 原样传给 `sh -c`；非 Unix 平台保留 lossy 转换（Windows 的 `cmd /C` 不变）。
- `run_command`、`run_command2` 的参数改为 `&[u8]`；`command_for_read`（`cmd | getline`）、`command_for_write`（`print | cmd`）去掉了 `from_utf8_lossy`，直接传字节。
- 调用方 `src/interp.rs`（`RunCmd`/`RunCmd2`）和 `src/codegen/intrinsics.rs`（`run_cmd`/`run_cmd2`）改为 `cmd.with_bytes(runtime::run_command)` / `with_bytes(runtime::run_command2)`，不再经过 `Str::as_str()`。
- 新增单元测试 `command_line_keeps_raw_bytes`（Unix）：`system2`、`system`、`cmd | getline` 三条路径，命令行里的 `\xe9`、`\xff` 字节原样到达 shell。原有两个测试的参数改为字节串字面量。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 4（中低）：`system2()` 执行前不刷新输出

**位置**：`src/interp.rs:3482`（`RunCmd2`）、`src/codegen/intrinsics.rs:1044`（`run_cmd2`）

`system()` 执行前会调用 `write_files.flush_all()`（两个后端都会）。`system2()` 不会。

`system2` 捕获了命令的 stdout 和 stderr，所以终端上的输出顺序没有问题。但命令如果读取脚本刚写过的文件，会读到不完整的内容：

```awk
BEGIN { print "x" > "out.txt"; r = system2("wc -l < out.txt"); print r["stdout"] }
# 缓冲区还没写出，可能打印 0
```

在 POSIX awk 里，读取前应该先 `close("out.txt")`。但用户会预期 `system2` 和 `system` 的行为一致。

**修复建议**：`system2` 执行前也调用 `flush_all()`，写入失败时与 `system` 的处理方式一致。

**状态**：已修复（未编译、未运行测试）。

- `src/interp.rs`（`RunCmd2`）：执行命令前调用 `write_files.flush_all()`，写入失败时 `return Ok(0)` 静默退出，与 `RunCmd` 相同。
- `src/codegen/intrinsics.rs`：`run_cmd2` 增加 `rt` 参数（签名改为 `run_cmd2(rt_ty, str_ref_ty) -> map_ty`），执行前 `try_silent_abort!(rt, (*rt).core.write_files.flush_all())`，与 `run_cmd` 相同。
- `src/codegen/mod.rs`：`RunCmd2` 不再走 `unop`，改为传入 `runtime_val()` 和命令字符串调用 `run_cmd2`。
- 新增集成测试 `system2_flushes_output`（`tests/misc.rs`，两个后端都跑）：先 `print "x" > out`、`printf "y" > out`，再 `system2("cat out")`，断言 `stdout` 为 `x\ny`。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 5（中低）：`http_post` 的 body 被 lossy 转换

**位置**：`src/runtime/network.rs:287`

`let body_text = body.to_string();` 用的是 `Display`，实际调用的是 `from_utf8_lossy`。body 如果不是 UTF-8（例如 GBK 文本或二进制数据），发出去的内容会被改写。`str_impl.rs` 里的注释说 `as_str` 只用在“文本离开 zawk”的地方，但 HTTP body 应该是字节，不能当作必须是 UTF-8 的文本。

另外，判断是否发 `application/json` 时没有先 `trim`，所以前面带换行的 JSON（`"\n{...}"`）会按 `text/plain` 发出。

**修复建议**：用 `body.with_bytes(|bs| bs.to_vec())` 作为 body。判断 JSON 时先 `trim_ascii()`。

**状态**：已修复（未编译、未运行测试）。

- `src/runtime/network.rs`（`http_post`）：body 改为 `body.with_bytes(|bs| bs.to_vec())`，按原始字节发送。
- 新增 `guess_body_content_type(&[u8])`：先 `trim_ascii()`，再判断是否以 `{…}` 或 `[…]` 包围，是则 `application/json`，否则 `text/plain`。用户自己设置了 `Content-Type` 时不变。
- 新增单元测试 `test_guess_body_content_type`：覆盖普通 JSON、前后带换行的 JSON 数组、单个 `{`、表单文本、GBK 字节。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 6（低）：数据库参数数组有空洞时，参数会前移

**位置**：`src/runtime/sqlite.rs:73`（`positional_params`，MySQL 和 PostgreSQL 也用它）

`positional_params` 先给键排序，再依次绑定。如果参数数组是 `p[1]="a"; p[3]="c"`（`p[2]` 没有赋值），`p[3]` 会被绑定到第 2 个占位符，查询静默地用错了参数。

**修复建议**：键不是 `1..n` 时给出警告，或者把空位绑定为 NULL（与 `to_csv` 补空位的做法一致）。

**状态**：已修复（未编译、未运行测试）。采用“空位绑定为 NULL”的方案。

- `src/runtime/csv.rs`：`csv_positions` 改为 `pub(crate)`，复用 `to_csv` 补空位的规则（键从 1 开始时按 `1..=max` 排位，缺的位置为 `None`；有 `<= 0` 的键或空洞超过 `MAX_CSV_GAP` 时仍只排序）。
- `src/runtime/sqlite.rs`：`positional_params` 返回 `Vec<Option<String>>`，`None` 表示空位；`sqlite_params` 把 `None` 绑定为 `Value::Null`。
- `src/runtime/mysql.rs`：`None` 和 `--null` 文本一样绑定为 `Value::NULL`。
- `src/runtime/postgres.rs`：`PgParam.text` 改为 `Option<String>`，`None` 时 `encode` 返回 `IsNull::Yes`；错误信息里用 `as_deref().unwrap_or("")`。
- 新增单元测试 `test_params_with_holes_keep_positions`（`sqlite.rs`）：`p[1]="a"; p[3]="c"` 得到 `[Some("a"), None, Some("c")]`，`SELECT ?1, typeof(?2), ?3` 返回 `a,null,c`。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 7（低）：`-o parquet` 不处理重复列名

**位置**：`src/runtime/parquet_writer.rs:323`（`parse_header`）

`-o avro`（`avro_writer.rs:242`）和 `-o jsonl`（测试 `jsonl_duplicate_names`）会给重复列名改名。`-o parquet` 会把重复的名字原样写进 schema。Spark 读取这样的文件会报 “Found duplicate column(s)”，pandas 和 pyarrow 的表现也不稳定。

**修复建议**：采用 avro 的规则，保留第一个，其余的改名。

**状态**：已修复（未编译、未运行测试）。采用 avro 的规则。

- `src/runtime/parquet_writer.rs`：新增 `unique_names`，`parse_header` 解析完各列后用它去重。重复的名字保留第一个，其余改名为 `a_2`、`a_3`……，跳过表头里其他列已经用掉的名字（表头里本来就有的 `a_2` 保留原名）。与 avro 不同的是，parquet 列名不需要改成合法标识符，所以只做去重，不替换字符。空列名先补成 `cN`，再参与去重。
- 新增单元测试 `duplicate_names_are_renamed`：`a, a:int64, a_2, (空), c2, a` 得到 `a, a_3, a_2, c4, c2, a_4`，类型保持不变；`(空), c1` 得到 `c1, c1_2`；写出的文件读回来是 `{id: "1", id_2: 2}`。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 8（低）：`system()` 启动失败时返回 errno 值

**位置**：`src/runtime/command.rs:84`

`run_command_inner(cmd).unwrap_or_else(|e| e.raw_os_error()...)`：`sh` 无法启动时（这种情况很少），`system()` 返回 errno 的值（例如 2）。脚本会把它当成命令的退出码，并且不会设置 `ERRNO`。

`system2` 已经改成返回 127 并设置 `ERRNO`（0.7.0 的 CHANGELOG）。**修复建议**：`system()` 采用同样的处理，返回 -1 或 127，并调用 `set_errno_io`。

**状态**：已修复（未编译、未运行测试）。采用 127，与 `system2` 的 `code` 一致，也和 C 库 `system(3)` 在 shell 无法执行时的返回值相同。

- `src/runtime/command.rs`：`run_command` 启动失败时调用 `set_errno_io` 并返回 127，不再返回 `raw_os_error()`。常量 `SYSTEM2_START_FAILURE` 改名为 `START_FAILURE`，由 `system` 和 `system2` 共用。
- 新增单元测试 `system_start_failure_returns_127_and_sets_errno`：命令行含 NUL 字节，启动失败，`system` 返回 127，`ERRNO` 非空。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

### 问题 9（低）：部分字符串函数不受 `-b` 影响

**位置**：`src/runtime/text_funcs.rs:113`（`units`），以及 `left`/`right`/`translate`/`reverse`、`Str::trim_sides`（`str_impl.rs:633`）

`length`、`substr`、`index`、`printf` 在 `-b`（`characters_as_bytes`）下都按字节计数。但 `left(s, n)`、`right(s, n)`、`translate`、`reverse` 和 `trim` 的右侧回退始终按 UTF-8 字符计数。于是在 `-b` 下，`left(s, 1)` 和 `substr(s, 1, 1)` 的结果不同。

**修复建议**：在 `units` 和 `reverse` 里判断 `characters_as_bytes()`，为真时按单字节切分。或者在文档里写明这些函数不受 `-b` 影响。

**状态**：已修复（未编译、未运行测试）。采用第一种方案：`-b` 下这些函数按字节处理。

- `src/runtime/text_funcs.rs`：`units` 在 `-b` 下把每个字节当成一个字符，`left`、`right`、`translate` 随之按字节计数；`reverse` 在 `-b` 下直接反转字节。另外，`units` 和 `reverse` 原来把非法 UTF-8 序列逐字节拆开，现在一个非法序列算一个字符（与 `length()`、`substr()` 相同，见问题 10），例如 `left("é\344\275a", 2)` 和 `substr` 一样得到 `é\344\275`。为了在单元测试里不改全局开关，各函数拆出带 `as_bytes` 参数的 `*_with` 版本。
- `src/runtime/str_impl.rs`：`Str::trim_sides` 在 `-b` 下改用新函数 `trim_bytes`：`pat` 是字节集合，`" "` 去掉 ASCII 空白（包括 `\v`）。不在 `-b` 下时行为不变。
- 新增单元测试 `units_follow_length`、`characters_as_bytes_count_bytes`、`trim_bytes_works_on_bytes`，`reverse_keeps_characters` 加了非法序列和 `-b` 的用例；新增集成测试 `text_functions_follow_char_units`（`tests/misc.rs`，两个后端，带和不带 `-b`）。
- `CHANGELOG.md` 的 Unreleased 加了一条说明（标为 Behavior change）。

### 问题 10（低）：含非法 UTF-8 时，`printf` 宽度和 `length()` 不一致

**位置**：`src/runtime/printf.rs:361`（`char_count`）

对于 `"é\xff"` 这样的字符串，`length()` 是 2（一个非法序列算一个字符，见 `str_impl::char_count`），`printf "%5s"` 却把整个字符串当字节计数，算出 3。补齐的空格数和 `length()` 推算的不一样。

**修复建议**：`printf` 里的 `char_count` 和 `take_chars` 改用 `str_impl::char_count` 和 `char_byte_range` 的规则，这样全项目只有一种“字符”的定义。

**状态**：已修复（未编译、未运行测试）。

- `src/runtime/printf.rs`：`char_count` 直接调用 `str_impl::char_count`（已处理 `-b` 和非法序列）；`take_chars` 用 `str_impl::char_byte_range(bs, 0, 0, 0, n)` 求前 `n` 个字符的字节范围，ASCII 和 `-b` 走按字节的快速路径。`%s` 的宽度/精度和 `%c` 取字符串首字符都随之改变：`printf "%5s", "é\377"` 现在补 3 个空格。
- `src/runtime/str_impl.rs`：`char_byte_range` 改为 `pub(crate)`。
- 新增单元测试 `widths_count_chars_as_length_does`；集成测试 `text_functions_follow_char_units` 也覆盖了这一点（`index(sprintf("%5s", "é\377"), "é")` 不带 `-b` 为 4，带 `-b` 为 3）。
- `CHANGELOG.md` 的 Unreleased 加了一条说明。

## 4. 检查过、没有发现问题的地方

- **`auto_parallel.rs`**：
  - 求和变量限定为小整数（`SUMMAND_BOUND`），避免浮点加法顺序造成差异；
  - 每条记录重新定义的变量（`defines_first`）判定正确；
  - `PROCINFO["sorted_in"]` 的支配关系（`OrderWalk`）判定正确；
  - `RSTART`/`RLENGTH` 的状态跟踪正确。
- **`printf`**：`fixed_small` 的误差界（2^40 和 1/1024）、`*` 宽度为负时左对齐、千位分组时补零、超出 i64 范围的值按 `%.0f` 或 `%g` 输出。
- **`decimal`**：`divide_round` 在 `shift` 为正和为负两种情况下的中点判定，以及负 scale 的处理。
- **`date_time`**：`date_diff` 只计完整的月，`local_timestamp` 处理 DST 重叠和缺口，`try_date_trunc` 按当时的偏移截断。
- **`writers.rs`**：FdPool 淘汰文件后用追加模式重新打开；显式 `close` 之后，再次 `>` 会截断文件。
- **最近修复的 getline**：redirection 在下标之前求值，且只求值一次（`cfg.rs:2337`），覆盖了 `from` 有副作用或下标有副作用两种情况。
- **`to_csv`**：加引号的规则与 `escape_csv` 一致；只有一个空字段时输出 `""`；`from_csv` 能正确读回。
- **`substr` 的非整数参数**：与 gawk 5.1.0 一致，例如 `substr("abcdef", 2.7, 2)` 是 `bc`，`substr("abcdef", -0.6, 2)` 是 `ab`。
- **段落模式**：单字符 FS 加 `\n` 作为字段分隔符（`update_split_fs`），解释器和 Cranelift 后端都用 `split_fs`。

## 5. 建议补充的测试

4 项都已补上（未编译、未运行测试）。

| # | 测试 | 位置 | 状态 |
|---|---|---|---|
| 1 | `a[length(d)] = asort(s, d)`、`a[length(t)] = asorti(s, t)` 等 5 个用例，期望输出由 gawk 生成 | `tests/compat/cases/03-evaluation-order-builtins.awk:15-19` | 已补充 |
| 2 | 主读取器的字段与 `split_record` 的结果相同，含 `id,size 5" screen,price`、`"say "hi" now",x` | `src/runtime/splitter/batch.rs:3727`（`csv_reassigned_record_splits_like_input`） | 已补充 |
| 3 | `print "x" > f; system2("cat " f)` 能读到 `x`（两个后端） | `tests/misc.rs:2100`（`system2_flushes_output`） | 已补充 |
| 4 | 带 `\xe9` 字节的命令原样传给 `sh`（只在 Unix 上运行） | `src/runtime/command.rs:260`（`command_line_keeps_raw_bytes`） | 已补充 |

说明：

1. 除了 `length(...)` 作下标，还覆盖了 `d[1]` 作下标，以及只传一个参数的 `asort`。用本机 gawk 5.1.0 重新跑过，输出与 `03-evaluation-order-builtins.out` 完全一致。
2. 除了直接断言这两条记录的拆分结果，还对 13 条记录分别以 `\n` 和 `\r\n` 结尾做了比对，并用 512/7/3 三种块大小覆盖 SIMD 扫描和 lenient 扫描两条路径。
3. 先 `print "x"` 再 `printf "y"`，断言 `system2` 的 `stdout` 为 `x\ny`。
4. 没有用 `od`，改为直接比较字节：`system2` 断言 `stdout` 是 `caf\xe9.txt`；`system` 让 shell 用 `test` 比较字节 0xe9，退出码为 0；`cmd | getline` 读回 `\xe9\xff`。三条路径中，只要有字节被替换成 U+FFFD，测试就会失败。

## 6. 测试方法（需要在有 Rust 工具链的机器上执行）

本机没有按要求编译和运行。修复以上问题后，请按 AGENTS.md 执行：

```sh
cargo av                    # 即 cargo check --all-targets --profile agent
cargo at                    # 即 cargo test --profile agent（不要加 --release）
cargo test --profile agent --test compat                  # gawk 兼容性用例
tests/compat/regen.sh --check                             # 校验 .out 与本机 gawk 一致
cargo build --profile agent && ./target/agent/zawk 'BEGIN { s[1]="b"; s[2]="a"; s[3]="c"; a[length(d)] = asort(s, d); for (k in a) print k, a[k] }'
# 期望输出：3 3
printf 'id,size 5" screen,price\n' | ./target/agent/zawk -i csv '{ n = NF; $0 = $0; print n, NF, $2 }'
# 期望输出：3 3 size 5" screen
```
