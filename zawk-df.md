# zawk DataFrame 支持：调研、方案与实现

> 状态：已实现（P0/P1/P2），实现说明见文末「13. 实现说明」；用户文档见 `website/docs/stdlib/dataframe.md`
> 日期：2026-11-19
> 目标：给 zawk 增加 DataFrame 能力（加载 / 过滤 / 分组聚合 / 排序 / Join / 透视 / 输出），**同时保持最大的 AWK/gawk 兼容性**：现有 AWK 程序的语义、性能和输出不变，不引入新语法。

---

## 1. 背景与现状（代码调研结论）

zawk 已经具备 DataFrame 所需的大部分"零件"，只是它们分散在流式模型里：

| 能力 | 现有位置 | 对 DataFrame 的意义 |
|---|---|---|
| 类型系统 | `src/compile.rs` 的 `Ty`：`Int/Float/Str/MapXxx/Iter/Null`，类型推断 + Cranelift JIT | **没有"对象/句柄"类型**，也不支持 `a[i][j]`；新增 `Ty` 代价极高（cfg、dataflow、bytecode、codegen、interp、lint 全部要改） |
| 内置函数注册 | `src/builtins.rs`（`Function` 枚举、名称表、签名 `(args, ret)`、arity、副作用）、`src/display.rs`、`src/compile.rs`、`src/cfg.rs`、`src/interp.rs`、`src/codegen/intrinsics.rs`、`src/function_docs.rs` | 新函数族按此模式接入 |
| 用户函数遮蔽 stdlib | `src/cfg.rs:570`、`src/cfg.rs:3740`：用户自定义函数会遮蔽同名 zawk stdlib 函数（POSIX/gawk 内置除外） | 新增 `df_*` 函数**不会破坏**已定义同名函数的老脚本 |
| 输入格式 | `src/runtime/splitter/`：csv/tsv/json/jsonl/parquet/avro/xlsx/markdown/database/grok | DataFrame 的加载器直接复用这些 reader |
| 列名 | `-H` + `FI` 数组（`$FI["age"]`） | DataFrame 列名语义与 `FI` 保持一致 |
| 输出格式 | `src/runtime/writers.rs`：csv/tsv/jsonl/markdown/table/parquet/avro/xlsx，`name:TYPE` 头部 | DataFrame 输出直接复用 `-o` writer 与类型后缀约定 |
| 类型推断 | `src/runtime/typed_value.rs`（`Inferred`、`parse_int`、日期/时间戳） | 列类型推断复用 |
| 统计 | `src/runtime/stats.rs`、`array_stats.rs`（`_sum/_mean/_median/_quantile/_stddev/_mode/...`，空值跳过） | 聚合函数直接复用，保证与数组统计**结果一致** |
| 排序顺序 | `src/runtime/sorted_in.rs`（`@ind_str_asc`、`@val_num_desc`...）、`compare.rs` | 排序/比较规则与 AWK 一致（strnum 语义） |
| 句柄式资源 | `conn_pool.rs`（数据库连接按 URL 字符串标识）、Bloom filter 按 group 名、`runtime/mod.rs` 的 `Registry<T>` | **"字符串句柄 + 运行时注册表"已是项目惯例** |
| 表格探查 | `src/inspect.rs`（schema、头尾行、统计） | 可复用为 `df_describe` |
| 并行 | `-p r/-p f`，`auto_parallel.rs`、lint W301~W306 | DataFrame 是全局有状态对象，需要并行规则 |

依赖现状：`Cargo.lock` 中**没有** arrow / polars / datafusion / duckdb；`parquet` 以 `default-features = false` 引入（未开 arrow 特性）；release 配置对多个大依赖使用 `opt-level = "s"`，说明项目**很在意二进制体积**。

---

## 2. 设计原则（兼容性优先）

1. **零新语法**。不引入 `df.filter(...)`、`df["col"]`、`a[i][j]`、方法调用、lambda。所有能力都是普通函数调用，任何 AWK 解析器都能解析（gawk 遇到会报"函数未定义"，而不是语法错误）。
2. **零新类型**。DataFrame 在 AWK 层就是一个**字符串句柄**（如 `"df#1"`），类型推断视之为 `Str`。不改 `Ty`、JIT、bytecode。
3. **与 AWK 数组互通**。行/列/结果都可以转成普通 AWK 数组（`map[int]str`、`map[str]str`），`for (k in arr)`、`length()`、`asort()` 照常工作。
4. **与现有语义对齐**：
   - 数值比较遵守 strnum 规则（输入来的 `"10"` 与 `9` 按数值比）；
   - 空值 = 空字符串（或 `--null` 文本），聚合时跳过（与 `_sum/_mean` 一致）；
   - 数字输出遵守 `OFMT`/`CONVFMT`，`print` 时遵守 `OFS`/`ORS` 和 `-o` 格式；
   - 列名、`:TYPE` 后缀与 `FI`、`-o parquet/avro/jsonl` 一致；
   - 排序顺序名沿用 `PROCINFO["sorted_in"]` 的 `@ind_*/@val_*` 规则。
5. **不改变不使用 DataFrame 的程序**：主循环、字段切分、投影下推、并行、性能全部不受影响；DataFrame 运行时惰性初始化。
6. **流式优先**：AWK 的主战场是流式。DataFrame 是补充（END 中的分组、Join、透视、排序），不是替代主循环。
7. **不过度依赖**：默认实现不引入 Polars/Arrow 等重量级依赖；可选重型后端走 cargo feature。

---

## 3. 确认方案

| 方案 | 描述 | 优点 | 缺点 | 结论 |
|---|---|---|---|---|
| **字符串句柄 + 原生 Rust 列存 + `df_*` 函数族** | 运行时注册表保存 `DataFrame`，函数按句柄操作 | 零语法/类型改动；完全复用 reader/writer/stats/sorted_in；语义可严格对齐 AWK；体积小 | 需自写算子（filter/group/join/sort/pivot）；表达式需用字符串 DSL | ✅ **推荐（主方案）** |

---

## 4. 用户视角 API（推荐方案）

### 4.1 句柄模型

在 zawk 的 scalar 和 array 类型之外，增加一种新的结构类型 `dataframe`，表示 DataFrame 句柄。

```awk
d = df_read("orders.csv")          # d == "df#1"（字符串句柄）
print typeof(d)                    # "dataframe"，新的结构类型
df_free(d)                         # 显式释放（可选，程序结束自动释放）
```

- 句柄格式：`df#<n>`，`n` 为进程内自增 id；非法句柄 → 运行时错误 `invalid dataframe handle "xxx"`（用 `try ... else` 可捕获）。
- 所有 `df_*` 变换**返回新句柄**（不可变语义），避免别名引起的意外；`df_free` 释放内存。
- 名字前缀统一用 `df_`，不与 POSIX/gawk 内置冲突；用户自定义同名函数会遮蔽它（已有机制），老脚本不受影响。
- **注意**：变量名不能是 `f`、`r`（f-string / raw 保留），文档示例统一用 `d`、`t`、`g`。

### 4.2 构造

| 函数 | 说明 |
|---|---|
| `df_read(path[, format[, opts]])` | 读文件/URL：csv、tsv、json、jsonl、parquet、avro、xlsx(`book.xlsx#Sheet2`)、markdown、`sqlite://...#table`、`mysql://`、`postgres://`、`http(s)://`、`s3://`、压缩文件；格式默认按扩展名/魔数推断（复用 `main.rs` 的推断逻辑与 `inspect.rs::extension_format`） |
| `df_new("name,age:int,city")` | 空表 + schema（`:TYPE` 后缀与 `-o parquet/avro` 一致） |
| `df_append(d, v1, v2, ...)` | 追加一行（主循环里收集：`{ df_append(d, $FI["name"], $FI["age"]) }`） |
| `df_append_record(d)` | 追加当前记录 `$1..$NF`（列名取自 `FI`/首行；`-p` 主循环中没有 `-H` 且表无列时报错，因为首条到达的记录不一定是首行；多个输入文件时第二个文件的表头作为数据行，与普通 awk 一致） |
| `df_from_array(arr[, "key,value"])` | AWK 数组 → 两列表（`for (k in arr)` 的典型场景） |

**主循环收集是关键的 AWK 风格用法**：

```awk
BEGIN { d = df_new("city,amount:float") }
$FI["status"] == "paid" { df_append(d, $FI["city"], $FI["amount"]) }
END {
    g = df_group(d, "city", "sum(amount) as total, count() as n")
    df_print(df_sort(g, "total desc"))
}
```

### 4.3 查询与变换

| 函数 | 说明 |
|---|---|
| `df_select(d, "a,b,c")` | 选列（支持 `a as x` 重命名） |
| `df_drop(d, "a,b")` | 删列 |
| `df_filter(d, expr)` | 过滤，`expr` 为表达式 DSL（见 §5） |
| `df_mutate(d, "x = a * 2, y = upper(name)")` | 新增/覆盖列 |
| `df_sort(d, "age desc, name")` | 稳定排序；可用 `@val_num_desc` 风格的顺序名 |
| `df_head(d, n)` / `df_tail(d, n)` / `df_slice(d, from, n)` | 截取 |
| `df_distinct(d[, "cols"])` | 去重（保持首次出现顺序，与 `uniq(arr, "keep")` 对齐） |
| `df_group(d, "keys", "aggs")` | 分组聚合：`sum/mean/min/max/median/quantile(c,q)/var/stddev/mode/count/count_distinct/first/last/concat(c,sep)` |
| `df_join(a, b, "on", [how])` | `inner/left/right/full/semi/anti`，`on` 支持 `id` 或 `id=user_id` |
| `df_concat(a, b)` | 纵向合并（按列名对齐） |
| `df_pivot(d, "row", "col", "agg(val)")` / `df_melt(d, "id_cols")` | 透视/逆透视 |

### 4.4 访问与互通（AWK 数组桥接）

| 函数 | 返回 |
|---|---|
| `df_nrow(d)` / `df_ncol(d)` | 行数/列数 |
| `df_columns(d, arr)` | 列名写入 `arr[1..n]`，返回 n |
| `df_get(d, row, col)` | 单元格（`col` 可为列名或 1 起始序号；行号 1 起始，与 `$1`/`NR` 一致） |
| `df_row(d, i, arr)` | 第 i 行写入 `arr[列名]`，返回列数 |
| `df_col(d, "age", arr)` | 一列写入 `arr[1..n]`，之后可用 `_median(arr)` 等 |
| `df_to_map(d, "key", "val", arr)` | 两列 → `arr[key] = val` |
| `df_schema(d)` | `"name:string,age:int64"` |

遍历全部行的惯用法（纯 AWK 循环，无新语法）：

```awk
for (i = 1; i <= df_nrow(d); i++) {
    df_row(d, i, row)
    print row["name"], row["age"]
}
```

### 4.5 输出

| 函数 | 说明 |
|---|---|
| `df_print(d[, format])` | 输出到 stdout；默认使用当前 `-o` 格式（未指定 `-o` 时用 `OFS`/`ORS` 文本输出，首行是表头）；`format` 可为 `csv/tsv/jsonl/markdown/table` |
| `df_write(d, path[, format])` | 写文件/`s3://`，含 parquet/avro/xlsx（复用 writers，带 `:TYPE` schema） |
| `df_describe(d)` | 返回/打印 schema + 统计摘要（复用 `inspect.rs`） |
| `df_to_csv(d)` / `df_to_json(d)` | 返回字符串 |

`df_print` 必须走与 `print` **相同的输出通道**（`writers::Registry` 的 stdout handle），以保证与 `print` 语句交错输出时顺序正确、`fflush`/`close` 语义一致、`-o` 格式一致。

---

## 5. 表达式 DSL（`df_filter` / `df_mutate` / `df_group`）

由于不能引入 lambda，过滤和计算用**字符串表达式**表达。设计要点：

1. **语法采用 AWK 表达式子集**，让用户零学习成本：
   - 列引用：裸列名 `age`、`$FI` 风格 `$"first name"`（含空格的列名）、序号 `$3`；
   - 运算：`+ - * / % ^`、`== != < <= > >=`、`~ !~`（正则，复用 `awk_regex` 与 `RegexCache`）、`&& || !`、`?:`、字符串连接（空格）、`in`（`city in ("Beijing","Shanghai")`）；
   - 函数：先支持一个白名单（`length substr index tolower toupper int sprintf trim ...`），其实现直接调用 `runtime` 里现有函数，保证与 AWK 调用**结果一致**；
   - 外部变量：`@name` 引用参数（`df_filter(d, "age > @min", "min", 30)`），**禁止字符串拼接构造表达式**（与 SQL 参数化同理，避免注入与引号地狱）。
2. **不复用 LALRPOP 主语法**（避免主语法冲突与编译期开销），在 `src/runtime/df/expr.rs` 手写一个小的 Pratt 解析器 → AST → 向量化求值（按列批量计算）。
3. **比较语义 = AWK strnum 语义**：来自输入的列值是 strnum；两边都"像数字"时按数值比较，否则按字符串比较（复用 `runtime::compare::looks_numeric`）。有类型的列（`int64/double`）直接数值比较。
4. **空值**：空串参与比较时同 AWK（`"" < 1` 为真）；聚合跳过空值（同 `_sum`）；提供 `is_null(col)`。
5. 表达式解析结果按字符串缓存（类似 `RegexCache`），主循环里重复调用不会重复解析。
6. 错误：解析错误给出位置与列名建议（复用 `strsim` 做 "did you mean"）。

---

## 6. 内部实现

### 6.1 模块布局

```
src/runtime/df/
  mod.rs        // DataFrame、Column、注册表 DfRegistry、句柄解析
  column.rs     // 列存：Int64 / Float64 / Str / Bool + null bitmap，strnum 标记
  io.rs         // 加载（复用 splitter / inspect 的 reader）与输出（复用 writers）
  expr.rs       // 表达式 DSL：lexer + Pratt parser + 向量化求值
  ops.rs        // select/filter/mutate/sort/distinct/slice/concat
  group.rs      // 分组聚合（hashbrown + foldhash），聚合器复用 stats.rs
  join.rs       // hash join（小表建哈希），semi/anti
  pivot.rs      // pivot / melt
```

### 6.2 数据结构

```rust
pub(crate) enum ColData {
    Int(Vec<i64>),
    Float(Vec<f64>),
    Str(Vec<Box<str>>),      // 第一版可简单实现；后续可换成 offsets + bytes 的 Arrow 风格布局
    Bool(Vec<bool>),
}
pub(crate) struct Column {
    name: String,
    decl: Option<String>,    // 原始 :TYPE 后缀（int64/decimal(12,2)/date...），输出 parquet/avro 时使用
    data: ColData,
    nulls: Option<Vec<bool>>,// 或 bitvec；None 表示无空值
    strnum: bool,            // 来自输入的未定型文本列：比较时走 strnum 规则
}
pub(crate) struct DataFrame { columns: Vec<Column>, nrows: usize }

pub(crate) struct DfRegistry { next: u64, frames: HashMap<u64, Rc<DataFrame>> }
```

- 不可变 + `Rc<DataFrame>`：`df_select` 等可以共享列（`Rc<Column>`），零拷贝；`df_append` 只对"构建中"的表做原地追加（引用计数为 1 时 `Rc::make_mut`）。
- **不要用 `Str<'a>` 存数据**：`Str` 带生命周期、和 JIT 的引用计数耦合；DataFrame 内部用拥有所有权的 Rust 字符串，跨边界时再转 `Str`（与 `sqlite_query` 返回值的做法一致）。
- 类型推断：加载 CSV/TSV 文本时，默认保持 `Str + strnum`（**与 AWK 读取字段的行为一致**）；提供 `df_read(path, "csv", "infer_types=1")` 或 `--infer-types` 才推断为 Int/Float/Date（复用 `typed_value::infer_type`）。Parquet/Avro/数据库读取时保留源类型。

### 6.3 注册表放在哪里

- 放到 `runtime` 的每线程运行时状态里（与 regex cache、文件句柄 Registry 同级，`interp.rs` 与 `codegen/intrinsics.rs` 都通过运行时指针访问），**惰性创建**，不使用 DataFrame 的程序零开销。
- 句柄解析：`df#123` → `u64`；采用 `BytesKey` 式零拷贝查找。

### 6.4 接入内置函数（每个 `df_*` 需要改的点）

参照 `ArrayMedian` 的接入路径：

1. `src/builtins.rs`：`Function` 枚举新增 `Df(DfOp)`（用子枚举，避免主枚举膨胀，参考 `Text(TextOp)`、`Decimal(DecOp)`）；名称表；签名 `type_sig`（句柄参数/返回值都是 `Str`，写入数组的函数参数为 `MapIntStr`/`MapStrStr`）；arity（可变参数的 `df_append`）；**副作用标记**（所有 `df_*` 都有副作用或依赖运行时状态，不能被常量折叠/死代码消除，参考 `builtin_assigns`、`cfg.rs:3657`）。
2. `src/display.rs`：名字显示。
3. `src/compile.rs` / `src/bytecode.rs` / `src/interp.rs`：解释器路径。
4. `src/codegen/intrinsics.rs`：JIT 路径（extern "C" 包装，只传句柄字符串和数组指针，避免 JIT 内联复杂逻辑）。
5. `src/function_docs.rs`：签名、说明、示例（`zawk functions` 与 `zawk llm` 自动可见）。
6. `src/lint.rs`：新诊断（见 §7.3）。
7. `tests/`：见 §9；`website/docs/stdlib/dataframe.md` 新文档页 + `llms.txt`、`skills/zawk` 同步。

> 建议：写一个宏/表驱动注册（`DfOp` 的名称、签名、arity、文档在同一处定义），否则 7 个文件 × 30 个函数非常容易漏。

### 6.5 I/O 复用

- 读：抽出 `inspect.rs` 中 `read_parquet/read_avro/read_xlsx/read_database/read_text` 的"产生行"逻辑为通用的 `RowSource` 迭代器（`Iterator<Item = Vec<Cell>>` + header），供 `inspect` 和 `df_read` 共用，避免第三份格式探测代码。
- 写：把 `writers.rs` 中各 Factory 做成"可写入任意 sink"的 API；`df_write(d, path, "parquet")` 内部构造 header 行（`name:TYPE`）+ 数据行，复用 TSV 转义 → 类型化的现有链路，保证 `-o parquet` 与 `df_write` 产出的文件**完全一致**。
- 远程（`http://`、`s3://`）与压缩复用现有 `open_input`/opendal。

---

## 7. 兼容性注意事项（重点）

### 7.1 语言层

- ✅ 不新增关键字、运算符、语法；`zawk fmt` 无需改动；gawk 能解析含 `df_*` 的脚本（运行时报函数未定义）。
- ✅ 不新增保留变量名（`--df` 模式的 `DF` 除外，且只在开启 `--df` 时注入；默认不保留，避免与用户变量 `DF` 冲突）。
- ✅ `df_*` 是 zawk stdlib 函数：用户自定义的同名函数优先（`cfg.rs:3740` 已有机制），老脚本不受影响。lint 提示遮蔽即可。
- ✅ 句柄是普通字符串：可存数组、可作为函数参数/返回值、可 `print`；`typeof(d)` 为 `"string"`。不要用数组作为句柄载体（`a[i][j]` 不支持且 AWK 数组不能作为函数返回值）。
- ⚠️ 不要让 `df_*` 隐式修改 `$0`、`NF`、`NR`、`FI`（这些在 zawk 中只读）；`df_row` 写入**用户给定的数组**。
- ⚠️ 不要使用 `f`、`r` 作为函数名或示例变量名。

### 7.2 语义对齐

| 项目 | 规则 |
|---|---|
| 下标 | 行号、列序号 **1 起始**（同 `$1`、`NR`、`split`） |
| 数值比较 | strnum 规则；类型列按数值 |
| 数字转字符串 | 取出到 AWK 时：整数原样；浮点按 `CONVFMT`，`print` 时按 `OFMT`（复用 `OfmtStr`） |
| 空值 | 空串 / `--null` 文本；聚合跳过；`count()` 计行，`count(col)` 计非空（同 SQL） |
| 统计 | `median/quantile/var/stddev/mode` 直接调用 `stats.rs`，与 `_median` 等结果**逐位一致** |
| 排序 | 稳定排序；字符串比较为字节序（同 `@ind_str_asc`），可选 `IGNORECASE` 感知；数值排序同 `@val_num_*` |
| 正则 | `~`/`!~` 使用 zawk 正则方言（`awk_regex`），与主程序一致 |
| 输出 | 尊重 `-o`、`OFS`、`ORS`、`--null`；与 `print` 共享 stdout 通道 |
| 字符计数 | `length/substr` 在 DSL 中按字符计数（与最近的 commit "count chars consistently" 一致） |
| 非 UTF-8 | 与 `tests/non_utf8.rs` 一致：按字节保存，不做 lossy 替换（存储用 `Box<[u8]>` 更稳妥） |

### 7.3 并行模式（`-p r` / `-p f`）

- 每个 worker 有独立运行时 → 独立注册表；主循环里 `df_append` 到 BEGIN 中创建的表，**结果不会自动合并**。
- 第一版策略：
  - `-p` 下主循环调用 `df_append*` → 新增 lint **W307**（"dataframe built in the main loop is per worker"），运行时同样打印警告（与 W302~W306 一致的处理方式）；
  - BEGIN/END 中使用 DataFrame 不受影响。
- 第二版：实现 worker 结束时把同名句柄的表按行合并（`--keep-order` 时按 chunk 顺序合并），类似现有对 END 可见标量/数组的聚合（`cross_stage.rs`、`auto_parallel.rs`）。
- `auto_parallel.rs` 自动并行判定：脚本含主循环 `df_append*` 时**不应自动开启并行**。

### 7.4 性能与内存

- 不使用 DataFrame 的脚本零开销（惰性注册表，无启动成本；注意 macOS 启动时间敏感，见 Cargo.toml 中 notify-rust 注释）。
- `df_read` 为全量加载：大文件提示内存风险；提供 `df_read(path, fmt, "columns=a,b;limit=1000")` 做列裁剪（parquet 可复用 `parquet::Projection`）与行限制。
- 可选内存上限 `--df-memory 2G`，超出给出清晰错误而不是 OOM。
- 句柄泄漏：循环中反复 `df_filter` 而不 `df_free` 会累积内存 → lint 提示（主循环中创建新 DataFrame 而无 `df_free`）。

### 7.5 安全

- 表达式 DSL 无执行命令/文件访问能力（白名单函数）。

### 7.6 其他

- 数据库 BLOB 仍按项目约定视为空字符串。
- `df_read` 读取 `xlsx#Sheet`、`json#$.path` 的 URL 片段语义与 `-i` 完全一致。
- 时间列：保持文本（`2024-01-15`、RFC 3339 UTC），与 parquet/avro 读取的文本表示一致；日期运算交给已有 `datetime` 函数。

---

## 8. 分阶段实现计划

| 阶段 | 内容 | 验收 |
|---|---|---|
| **P0 基础** | `runtime/df` 骨架、注册表、句柄；`df_new/df_append/df_append_record/df_read(csv,tsv,jsonl)/df_nrow/df_ncol/df_columns/df_get/df_row/df_col/df_print/df_free`；builtins/interp/JIT/docs 接入 | 两个后端（`tests/backend_diff.rs`）结果一致 |
| **P1 核心算子** | 表达式 DSL；`df_select/drop/filter/mutate/sort/head/tail/distinct/group`；聚合复用 stats | 与等价 AWK 数组写法输出一致（对照测试） |
| **P2 关系算子 + I/O** | `df_join/concat/pivot/melt`；`df_read` 支持 parquet/avro/xlsx/markdown/数据库/HTTP/S3；`df_write` 全格式；`df_describe` | 与 `-o parquet`、`inspect` 输出一致 |

---

## 9. 测试方案

**本仓库主机无 Rust 工具链，以下命令需在开发机（macOS arm64）上执行：**

```shell
# 每次修改后
cargo check --all-targets --profile agent      # 或 cargo av
# 运行测试（不要用 --release）
cargo test --profile agent                     # 或 cargo at
cargo test --profile agent df_                 # 只跑 dataframe 相关
# 冒烟测试用二进制
cargo build --profile agent
./target/agent/zawk 'BEGIN { d = df_read("tests/demo.csv"); df_print(df_head(d, 3)) }'
```

测试清单：

1. **单元测试**（`src/runtime/df/*`）：表达式解析/求值、strnum 比较、空值聚合、稳定排序、各 join 类型、pivot/melt、句柄错误。
2. **对照测试**（新建 `tests/dataframe.rs`）：同一需求分别用 AWK 数组写法和 `df_*` 写法，断言输出相同，例如：
   ```shell
   zawk -i csv -H '{ s[$FI["city"]] += $FI["amount"] } END { PROCINFO["sorted_in"]="@ind_str_asc"; for (c in s) print c, s[c] }' orders.csv
   zawk -i csv -H 'END { df_print(df_sort(df_group(df_read(FILENAME), "city", "sum(amount)"), "city"), "tsv") }' orders.csv
   ```
3. **后端一致性**：`tests/backend_diff.rs` 增加 `df_*` 用例（解释器 vs Cranelift）。
4. **格式往返**：`df_write(parquet/avro/xlsx)` → `df_read` → 比对；与 `-o parquet` 生成的文件 schema 比对。
5. **兼容性回归**：`tests/compat` 全量通过；新增用例：用户自定义 `df_read` 函数时遮蔽内置；`print` 与 `df_print` 交错输出顺序；`-o csv/jsonl/table` 下 `df_print`。
6. **并行**：`-p r` 下主循环 `df_append` 触发 W307 警告；`zawk lint -pr` 输出 W307。
7. **非 UTF-8**：`tests/non_utf8.rs` 增加 `df_read` 字节保真用例。
8. **数据库/远程**（需 `docker compose up -d`）：`df_read("mysql://root:123456@127.0.0.1:3306/test#people")`、`df_read("postgres://postgres:123456@127.0.0.1:15432/demo_db#people")`、`df_read("http://127.0.0.1:18080/...")`、`df_read("s3://mj-artifacts/demo.csv")`，并与 `people` 表期望数据（6 行）比对。

---

## 10. 示例集合（文档用）

```awk
# 1. 分组 Top-N
END {
    d = df_read("orders.csv")
    g = df_group(df_filter(d, "status == \"paid\""), "city", "sum(amount) as total, count() as n")
    df_print(df_head(df_sort(g, "total desc"), 10), "table")
}

# 2. 两个文件 Join（替代经典的 NR==FNR 写法，但经典写法依然可用）
BEGIN {
    u = df_read("users.csv"); o = df_read("orders.csv")
    df_print(df_join(o, u, "user_id=id", "left"))
}

# 3. 主循环收集 + 透视
BEGIN { d = df_new("day,level,n:int") }
{ df_append(d, substr($1, 1, 10), $3, 1) }
END { df_print(df_pivot(d, "day", "level", "sum(n)"), "md") }


# 4. 回到 AWK 数组
END {
    df_col(df_read("lat.csv"), "latency", lat)
    print _median(lat), _quantile(lat, 0.99)
}
```

---

## 11. 问题确认

1. 句柄格式: `df#1`
2. `df_print` 在未指定 `-o` 时：默认 `OFS` 分隔文本（更 AWK）还是 `table`（更友好）？建议：默认 OFS 文本，`table` 需显式指定，保持脚本输出可被下游 awk 处理。 
3. CSV 加载时默认是否推断类型？默认不推断（strnum，与 AWK 行为一致），`infer_types=1` 开启。

---

## 12. 本期暂不考虑的实现

* dataframe的 SQL 不需要支持
* polars 等集成，这个也不用需要支持

---

## 13. 实现说明（与设计稿的差异）

实现位于 `src/runtime/df/`（`mod.rs` 注册表/句柄/分发、`column.rs` 列存、`expr.rs` 表达式 DSL、`ops.rs`、`group.rs`、`join.rs`、`pivot.rs`、`io.rs`），用户文档为 `website/docs/stdlib/dataframe.md`，`info/stdlib.md`、`llms.txt`、`skills/zawk/SKILL.md`、`README.md`、`CHANGELOG.md` 已同步。

接入方式（§6.4）：

* `builtins::Function::Df(DfOp)`：所有只收发标量的 `df_*` 共用一个变体，参数个数可变（`DfOp::arg_range` 在 `cfg.rs` 检查），返回 `Str` 或 `Int`（`DfOp::returns_int`），字节码为 `DfStr`/`DfInt`，JIT 统一走 `df_call_str`/`df_call_int` 两个 intrinsic（字符串参数数组 + 运行时指针），新增函数只需改 `runtime/df` 与 `function_docs.rs`。
* 读写 AWK 数组的函数单独成变体：`DfColumns`、`DfCol`（`map[int]str`）、`DfRow`、`DfToMap`（`map[str]str`）、`DfFromArray`。
* `df_append_record(d)` 在 `cfg.rs` 中改写为 `df_append_record(d, join_tsv(1, NF))`，字段投影分析因此知道需要全部字段。
* 有副作用的调用即使结果未使用也会生成（`compile.rs`），并标记在 `builtin_assigns`；DataFrame 结果在 strnum 分析中视为可能的 strnum（与 `sqlite_query` 结果一致）。

与设计稿的差异：

1. **注册表是进程级的**（`Mutex<HashMap<u64, Arc<DataFrame>>>`），不放在每线程运行时：句柄在 BEGIN、worker、END 之间都有效，`-p` 下主循环 `df_append` 的行会进入同一张表（顺序不确定），因此 W307 的含义是“行顺序不确定”而不是“结果不合并”；第二版的“按 worker 合并”不再需要。
2. **`typeof(d)` 返回 `"dataframe"`**（按 §4.1；§7.1 的 `"string"` 作废），表被 `df_free` 后返回 `"string"`。`type_of_strnum` intrinsic 不再标记为 ReadOnly。
3. 表达式按行求值（不是向量化），解析结果按文本缓存；`'单引号字符串'` 也可用，便于写在 awk 双引号字符串中。
4. `df_sort` 中 null（缺失值）排在升序最前（与空串最小一致），降序最后。
5. `df_group`/`df_pivot` 的分组按首次出现顺序输出；`df_pivot` 的列按值排序。
6. `df_join` 结果为左表全部列 + 右表非键列，重名列加后缀 `_right`；`right`/`full` 中只在右表出现的行，左表键列取右表的键。
7. `df_read` 读取 CSV/TSV 时按字节保存（非 UTF-8 不做替换）；其它格式复用 `inspect.rs` 的读取器（抽出 `RowSink` trait，`zawk inspect` 与 `df_read` 共用）。Parquet/Avro/数据库的源类型由值决定（数字列为 int64/double），不保留源 schema 的类型名。
8. `df_write` 通过 `-o` 的 writer（`JsonlWriter`/`ParquetWriter`/`AvroWriter`/`XlsxWriter`/`MarkdownWriter`/`TableWriter`）渲染，类型列带 `name:TYPE` 头；支持 `.gz` 压缩和 `s3://`（新增 `s3::put_object_bytes`）。
9. 暂未实现：`--df-memory` 内存上限、主循环中未 `df_free` 的泄漏 lint、`--df` 模式、IGNORECASE 感知的排序。

