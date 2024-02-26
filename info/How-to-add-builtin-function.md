# how to add a builtin function

### Declare function

* Declare function name in `pub enum Function ` of [src/builtins.rs](../src/builtins.rs)
* Bind function name in `static_map!` of [src/builtins.rs](../src/builtins.rs)
* Add function name in `pub(crate) fn arity(`(参数个数) function of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn type_sig(`(函数类型签名) of [src/builtins.rs](../src/builtins.rs)
* Add function name in `fn step(`(函数返回值类型) of [src/builtins.rs](../src/builtins.rs)
* Dynamic parameters in `fn call<'c>(` of [src/cfg.rs](../src/cfg.rs)

### Bind function with AWK compiler

* add display for `impl Display for Function`(字符串化) of [src/display.rs](../src/display.rs)
* Add function name in `fn builtin(`(内置指令集) of [src/compile.rs](../src/compile.rs)
* Add function signature in `pub(crate) enum Instr`(指令签名) of [src/bytecode.rs](../src/bytecode.rs)
* Add function name in `pub(crate) fn accum(`(清空器) of [src/bytecode.rs](../src/bytecode.rs)
* Add function name in `fn visit_ll(`(遍历器) of [src/dataflow.rs](../src/dataflow.rs)
* Add function name in `fn run_at(`(并发运行) of [src/interp.rs](../src/interp.rs) 并发时调用

### Function implementation

* Add function name in `fn gen_ll_inst` of [src/codegen/mod.rs](../src/codegen/mod.rs)
* register function in `register! {`(指令编译注册) of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Add function implementation: 可以考虑将功能实现放到runtime下的一个module中，如`date_time.rs`。

`uuid`(generator) implementation in [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)

```
pub(crate) unsafe extern "C" fn uuid(runtime: *mut c_void) -> U128 {
    let res = Str::from("demo");
    mem::transmute::<Str, U128>(res)
}
```

`fend`(transform) implementation in [src/runtime/str_impl.rs](../src/runtime/str_impl.rs)

```
    pub fn fend<'b>(&self) -> Str<'b> {
        let mut context = fend_core::Context::new();
        let expr = self.to_string();
        return match fend_core::evaluate(&expr, &mut context) {
            Ok(result) => {Str::from(result.get_main_result().to_string())}
            Err(error) => {Str::from(format!("FendError:{}",error))}
        }
    }
```

If you can not determine param type, such as min(param1, param2), please use `Str`.

### Dynamic Param Type

* Please check param type in `fn builtin(`(内置指令集) of [src/compile.rs](../src/compile.rs)  and match function
  signature in `pub(crate) enum Instr`(指令签名) of [src/bytecode.rs](../src/bytecode.rs).

### Custom Math function

* Declare function name in `pub enum FloatFunc ` of [src/builtins.rs](../src/builtins.rs)
* Implement relative logic in [src/builtins.rs](../src/builtins.rs)
* register function in `register! {` of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Declare FFI function `pub(crate) unsafe extern "C" fn _frawk_abs`
  of [src/codegen/intrinsics.rs](../src/codegen/intrinsics.rs)
* Add logic in `fn floatfunc(` of [src/codegen/clif.rs](../src/codegen/clif.rs)

### Resource functions

* File: getline https://www.gnu.org/software/gawk/manual/html_node/Getline.html

### Global variables

please refer `ENVIRON` as example.

* Declare variable name in `pub(crate) enum Variable {` of [src/builtins.rs](../src/builtins.rs)
* Change some logic in `impl From<Variable> for compile::Ty` of [src/builtins.rs](../src/builtins.rs)
* Implement logic in `fn default() -> Variables<'a>` of [src/builtins.rs](../src/builtins.rs)
* register variable `static_map!(` of [src/builtins.rs](../src/builtins.rs)
* add display in `impl Display for Variable`(字符串化) of [src/display.rs](../src/display.rs)
* Add variable support in `pub fn shuttle(&self, pid: Int)` of [src/interp.rs](../src/interp.rs)

### File format support

* text
* csv
* Apache Parquet - use dr to convert Parquet to CSV

### Diagnose support

To make diagnose convenient, we add `var_dump` and `log` functions.

```shell
$ frawk 'BEGIN { var_dump("hello"); log_debug("Hello Debug"); print "first", "second"; }' --out-file output.txt
```

**Tips**: To make diagnose and content output clear, you can use `--out-file` to redirect output to file,
and `var_dump()` and `log_debug()` always use standard output.

### Normal String formats

* URL(MapStrStr): `url("https://example.com/user/1")`, `url("jdbc:mysql://localhost:3306/test")`
* Data URL(MapStrStr): `data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")`
* Date(MapStrInt): `datetime("2023-12-11 13:13:13")`, https://github.com/waltzofpearls/dateparser
* Command line(MapIntStr): `shlex("ls -l")`, https://crates.io/crates/shlex
* Math expression(Float): `fend("1+2")`, https://github.com/printfn/fend
* Path(MapStrStr): `path("./demo.txt")`
* Semantic Versioning(MapStrStr): `semver("1.2.3-alpha")`, `semver("1.2.3-alpha.1+zstd.1.5.0")` return array
  with `major`, `minor`, `patch`, `pre`, `build` fields.

如果你要看返回的数据结构，可以使用var_dump函数，如`var_dump(semver("1.2.3-alpha.1+zstd.1.5.0"))`。

### UDF(User Defined Function)

* uuid : `uuid()`, `uuid("v7")`, ulid `print ulid()`, `snowflake(machine_id)`,
* array: `delete arr[1]`, `delete arr`, `length(arr)`, `n = asort(arr)`,
* array extension: 所有下划线开头的函数，只能用于数组，这个遵循Underscore.js的风格
    - `seq(start, end, step)`: seq命令兼容
    - `uniq(arr)`: IntMap<Str> -> IntMap<Str>, uniq命令行兼容
    - `n = asort(arr)`: gawk兼容
    - `_max(arr)`: IntIntMap -> Int, IntFloatMap -> Float
    - `_min(arr)`: 
    - `_sum(arr)`: 
    - `_mean(arr)`: 
    - `_join(arr, ",")` IntMap -> Str
* bool: `mkbool(s)`, such
  as `mkbool("true")`, `mkbool("false")`, `mkbool("1")`, `mkbool("0")`, `mkbool("0.0")` `mkbool("  0  ")`, `mkbool("Y")`, `mkbool("Yes")`, `mkbool("")`,`mkbool("✓")`
* reflection: `isarray(x)`, `typeof(x)` https://www.gnu.org/software/gawk/manual/html_node/Type-Functions.html
* i18n: `LC_MESSAGES`
* math: `abs`, `floor`, `ceil`, `round`, `fend("1+2")`, `min(1,2,3)`, `max("A","B")`, `float("11.2")`
* string:  Please regex express for `is_xxx()`、`contains()`、`start_with()`、`end_with()` functions.
    - strtonum: numeric value(十进制) `strtonum("0x11")`
    - trim: `trim($1)` or `trim($1, "[]()")`
    - truncate: `truncate($1, 10)` or `truncate($1, 10, "...")`
    - escape: `escape("sql", $1)`, such as json, csv,tsv, xml, html, sql, shell.
    - capitalize: `capitalize($1)`
    - camel_case??: `camel_case($1)`
    - kebab_case??: `kebab_case($1)`
    - snake_case??: `snake_case($1)`
    - title_case??: `title_case($1)`
    - shlex: parse command line
    - math: `isint()`, `isnum()`
    - mask: `mask("abc@example.com")`, `mask("186612347")`
    - pad:  `pad($1, 10, "*")` to `***hello**`, `pad_start($1, 10, "*")` to `***hello`, `pad_end($1, 10, "**")` to `hello***`,
    - strcmp: text compare `strcmp($1, $2)` return -1, 0, 1
* json: `from_json(json_text)`, `to_json(array)` nested not support
* csv: `from_csv(csv_text)`, `to_csv(array)`
* encoding: `hex`, `base32`(RFC4648 without
  padding), `base64`, `base64url`, `url`, `hex-base64`,`hex-base64url`, `base64-hex`,`base64url-hex`, such
  as `encode("base64", $1)`, `encode("url",$1)`, `decode("base64", $1)`, `encode("hex-base64",$1)`
* Digest(digest, hash): `md5`, `sha256`, `sha512`, `bcrypt`, `murmur3`, `xxh32`, `xxh64`, `blake3`, such
  as `digest("md5",$1)`, `digest("sha256",$1)`. Checksum: `crc32`, `adler32`
* crypto:
    - hmac: `hmac("HmacSHA256","your-secret-key", $1)` or `hmac("HmacSHA512","your-secret-key", $1)`
    - jwt: `jwt("HS256","your-secret-key", arr)`, `dejwt("your-secret-key", token)`.
      algorithm: `HS256`, `HS384`, `HS512`.
* parser: `url("http://example.com/demo?query=1")`, `data_url("data:text/plain;base64,SGVsbG8sIFdvcmxkIQ==")`
* KV: for Redis, and namespace is like `redis://localhost:6379/namespace`, or `redis://localhost:6379/0/namespace`. For
  NATS, namespace is like `nats://localhost:4222/bucket_name`, please use `nats kv add bucket_name` to create bucket
  first.
    - `kv_get(namespace, key)`
    - `kv_put(namespace, key, text)`
    - `kv_delete(namespace, key)`
    - `kv_clear(namespace)`
* Redis KV: use Map structure, `kv_get("redis://user:password@host:6379/db/namespace")`
* Events: `publish(namespace, body)`. To NATS, `publish("nats://host:4222/topic", body)`
* Network: `local_ip()`  `http_get(url,headers)`, `http_post(url, headers, body)`,
* S3 support: `s3_get(bucket, object_name) `, `s3_put(bucket, object_name, body)`, please supply
  ENV: `S3_ENDPOINT`, `S3_ACCESS_KEY_ID`, `S3_ACCESS_KEY_SECRET`, `S3_REGION`
* SQLite support: KV storage, `sqlite_query`, `sqlite_execute`
* MySQL
  support: `mysql_query("mysql://root:123456@localhost:3306/test", "select id, name from people")`, `mysql_execute`
* i18n: gettext, not support now.
* date time: utc by default
    - systime: current Unix time
    - strftime: https://docs.rs/chrono/latest/chrono/format/strftime/index.html
    - mktime https://docs.rs/dateparser/latest/dateparser/#accepted-date-formats
    - datetime: `datatime()`, `datetime(1621530000)["year"]`, `datetime("2020-02-02")["year"]`
* os: `whoami()`, `os()`, `arch()`, `os_family()`, `pwd()`, `user_home()`
* diagnose: dump and logging `var_dump(name)`, `log_debug()`, `log_info()`, `log_warn()`, `log_error()`

### References

* inflector: add String based inflections for Rust. Snake, kebab, train, camel, sentence, class, and title
  cases - https://github.com/whatisinternet/inflector
* Internationalization with gawk: https://www.gnu.org/software/gawk/manual/html_node/I18N-Example.html

# todo

* Apache Parquet Read: please use [dr](https://crates.io/crates/dr) to convert parquet to CSV file.
